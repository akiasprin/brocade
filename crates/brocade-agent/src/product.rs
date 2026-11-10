use std::path::Path;

/// An unavailable authority or unknown cleanup state cannot authorize a downgrade.
pub(crate) fn check_base_transition(args: &[String]) -> Result<(), String> {
    let options = crate::options::Options::parse(args.to_vec())?;
    ensure_base_ready(&options.state_dir)?;
    let response = crate::http::HttpClient::new(&options.server)?.request(
        "GET",
        "/agent/v1/product/base-ready",
        &options.token,
        None,
    )?;
    if response.status != 200 {
        return Err("Console 尚未确认 CVM 资源已全部清理，不能切换为基础版".into());
    }
    let value: serde_json::Value =
        serde_json::from_str(&response.body).map_err(|_| "Console 的降级检查响应无效")?;
    if value.get("base_ready").and_then(serde_json::Value::as_bool) != Some(true) {
        return Err("Console 未确认基础版切换条件".into());
    }
    Ok(())
}

/// Check the current executor before replacing it with a base Agent.
pub(crate) fn ensure_base_ready(state: &Path) -> Result<(), String> {
    for name in ["cvm-nics-desired.json", "cvm-entries-desired.json"] {
        let Some(bytes) = crate::fsutil::read_bounded(&state.join(name), 16 * 1024 * 1024)? else {
            continue;
        };
        let value: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| "CVM 执行记录无效，不能切换为基础版")?;
        let object = value.as_object().ok_or("CVM 执行记录必须是对象")?;
        for key in [
            "bindings",
            "entries",
            "tunnels",
            "deliveries",
            "fences",
            "budgets",
        ] {
            if let Some(value) = object.get(key) {
                let items = value.as_array().ok_or("CVM 执行记录的资源清单无效")?;
                if !items.is_empty() {
                    return Err("仍有 CVM 网卡、入口或网络预约，不能切换为基础版".into());
                }
            }
        }
    }
    if let Some(bytes) =
        crate::fsutil::read_bounded(Path::new("/opt/brocade/cvm/host.json"), 64 * 1024)?
    {
        let host: serde_json::Value =
            serde_json::from_slice(&bytes).map_err(|_| "CVM 宿主切换记录无效")?;
        if !host.is_object() || host.get("pending").is_some_and(|value| !value.is_null()) {
            return Err("CVM 宿主切换尚未完成".into());
        }
    }
    // Base retains only neutral checkpoint inspection, never Cube execution.
    let execution = std::env::var_os("BROCADE_CVM_STATE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| "/var/lib/brocade/cvm-cube".into());
    if execution
        .try_exists()
        .map_err(|_| "无法检查 CVM 执行状态")?
    {
        for (count, entry) in std::fs::read_dir(&execution)
            .map_err(|_| "无法读取 CVM 执行状态")?
            .enumerate()
        {
            if count >= 8192 {
                return Err("CVM 执行记录超出检查上限".into());
            }
            let path = entry.map_err(|_| "无法读取 CVM 执行记录")?.path();
            if path.extension().is_none_or(|suffix| suffix != "json") {
                continue;
            }
            let bytes =
                crate::fsutil::read_bounded(&path, 32 * 1024)?.ok_or("CVM 执行记录检查期间变化")?;
            let record: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|_| "CVM 执行记录无效")?;
            if record.get("deleted").and_then(serde_json::Value::as_bool) != Some(true) {
                return Err("CVM 实例尚未确认物理删除，不能启动基础版".into());
            }
        }
    }
    #[cfg(feature = "cvm")]
    {
        let _controller = crate::cvm::nics::controller_lock(state)
            .map_err(|_| "请先停止 CVM Agent，再检查基础版切换条件")?;
        if crate::cvm::maintenance()? {
            return Err("CVM 宿主切换尚未完成".into());
        }
        let mut config = std::env::var_os("BROCADE_CVM_CONFIG")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| "/etc/brocade/cvm-cube.json".into());
        if std::env::var_os("BROCADE_CVM_CONFIG").is_none()
            && !config.try_exists().map_err(|_| "无法检查 CVM 配置")?
        {
            let managed = std::path::PathBuf::from("/opt/brocade/cvm/current/generated/cube.json");
            if managed.try_exists().map_err(|_| "无法检查受管 CVM 配置")? {
                config = managed;
            }
        }
        let execution = std::env::var_os("BROCADE_CVM_STATE_DIR")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| "/var/lib/brocade/cvm-cube".into());
        if config.try_exists().map_err(|_| "无法检查 CVM 配置")? {
            brocade_cube::Backend::inspect_empty(&config, &execution)?;
        } else if std::env::var_os("BROCADE_CVM_CONFIG").is_some()
            || execution
                .try_exists()
                .map_err(|_| "无法检查 CVM 执行状态")?
        {
            return Err("CVM 配置缺失，无法证明执行栈已清空".into());
        }
    }
    Ok(())
}

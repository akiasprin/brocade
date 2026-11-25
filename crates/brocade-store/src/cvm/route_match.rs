use super::*;
use std::net::IpAddr;

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CvmRouteMatchProtocol {
    Tcp,
    Udp,
    Other,
}

#[derive(Debug, Deserialize)]
pub struct CvmRouteMatchRequest {
    pub destination: IpAddr,
    pub protocol: CvmRouteMatchProtocol,
    pub port: Option<u16>,
    pub routes: Option<ContainerRoutes>,
    pub network_edit: Option<CvmNetworkEditRequest>,
}

impl CvmRouteMatchRequest {
    fn flow(&self) -> Result<Option<(L4Protocol, u16)>> {
        if matches!(self.destination, IpAddr::V6(ip) if ip.to_ipv4_mapped().is_some()) {
            return Err(invalid(
                "不支持映射到 IPv6 的 IPv4 地址，请直接填写 IPv4 地址",
            ));
        }
        match (self.protocol, self.port) {
            (CvmRouteMatchProtocol::Tcp, Some(port @ 1..)) => Ok(Some((L4Protocol::Tcp, port))),
            (CvmRouteMatchProtocol::Udp, Some(port @ 1..)) => Ok(Some((L4Protocol::Udp, port))),
            (CvmRouteMatchProtocol::Other, None) => Ok(None),
            (CvmRouteMatchProtocol::Other, Some(_)) => Err(invalid("其他协议无需填写端口")),
            _ => Err(invalid("TCP／UDP 匹配需要填写 1–65535 的目的端口")),
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CvmRouteBlockReason {
    ChainRequiresTcpUdp,
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CvmRouteMatch {
    Route {
        route_index: usize,
        route: ContainerRoute,
        priority: u16,
        #[serde(skip_serializing_if = "Option::is_none")]
        blocked: Option<CvmRouteBlockReason>,
    },
    Default,
}

fn match_route(
    routing: &ContainerRoutes,
    execution: &ExecutionTarget,
    destination: IpAddr,
    flow: Option<(L4Protocol, u16)>,
) -> CvmRouteMatch {
    let Some((route_index, route, priority)) =
        routing.select_route_flow(destination, flow, execution)
    else {
        return CvmRouteMatch::Default;
    };
    CvmRouteMatch::Route {
        route_index,
        route: route.clone(),
        priority,
        blocked: (flow.is_none() && route.target.chain_id().is_some())
            .then_some(CvmRouteBlockReason::ChainRequiresTcpUdp),
    }
}

fn check_route_bounds(routes: &ContainerRoutes) -> Result<()> {
    // Bound the raw lists before normalization can remove duplicate entries.
    if routes.routes.len() > MAX_NETWORK_ROUTES
        || routes.priorities.len() > MAX_NETWORK_ROUTES
        || routes.dns.len() > 4
    {
        return Err(invalid(
            "路由和出口优先级最多各 256 条，DNS 服务器最多 4 个，请减少后重试",
        ));
    }
    Ok(())
}

impl PgStore {
    /// Evaluate saved desired routes or a rollback-only compile preview of a proposed table.
    /// This only classifies the destination; it never contacts a node or tests connectivity.
    pub async fn match_cvm_route(
        &self,
        actor: &AdminContext,
        id: &str,
        request: CvmRouteMatchRequest,
    ) -> Result<CvmRouteMatch> {
        if !valid_id(id) {
            return Err(invalid("容器标识格式不正确"));
        }
        let flow = request.flow()?;
        if let Some(edit) = request.network_edit {
            if request.routes.is_some() {
                return Err(invalid("请仅提交暂存路由或完整网络草稿，不要同时提交"));
            }
            return self
                .match_cvm_network_preview(actor, id, request.destination, flow, edit)
                .await;
        }
        let (tenant, raw): (String, serde_json::Value) =
            sqlx::query_as("SELECT tenant_id,desired FROM cvm_instances WHERE id=$1")
                .bind(id)
                .fetch_optional(&self.pool)
                .await?
                .ok_or_else(|| StoreError::NotFound("CVM".into()))?;
        require_manager(actor, &tenant)?;
        let desired: InstanceDesired = serde_json::from_value(raw)?;
        if desired.power == PowerState::Deleted {
            return Err(invalid("容器已进入删除流程"));
        }
        let Some(routes) = request.routes else {
            return Ok(match_route(
                &desired.spec.routing,
                &desired.spec.execution,
                request.destination,
                flow,
            ));
        };
        check_route_bounds(&routes)?;
        if routes.revision != desired.spec.routing.revision {
            return Err(StoreError::Conflict("路由表已变化，请刷新后操作".into()));
        }
        let mut normalized = routes.clone();
        normalized.normalize();
        if normalized == desired.spec.routing {
            return Ok(match_route(
                &routes,
                &desired.spec.execution,
                request.destination,
                flow,
            ));
        }
        self.match_cvm_network_preview(
            actor,
            id,
            request.destination,
            flow,
            CvmNetworkEditRequest {
                request_id: String::new(),
                expected_generation: desired.generation,
                mounts: Vec::new(),
                detach_interface_ids: Vec::new(),
                entry_additions: Vec::new(),
                entry_releases: Vec::new(),
                routes: Some(CvmNetworkRoutesWrite {
                    expected_revision: routes.revision,
                    routes,
                }),
            },
        )
        .await
    }

    async fn match_cvm_network_preview(
        &self,
        actor: &AdminContext,
        id: &str,
        destination: IpAddr,
        flow: Option<(L4Protocol, u16)>,
        mut edit: CvmNetworkEditRequest,
    ) -> Result<CvmRouteMatch> {
        let input = edit
            .routes
            .as_ref()
            .map(|table| {
                check_route_bounds(&table.routes)?;
                Ok::<_, StoreError>(table.routes.routes.clone())
            })
            .transpose()?;
        // A match must validate against the current generation, never replay a save receipt.
        edit.request_id = new_id("route-match")?;
        let preview = self
            .edit_cvm_network(
                actor,
                id,
                edit,
                CvmMutationReview {
                    preview: true,
                    ..CvmMutationReview::default()
                },
            )
            .await?;
        let spec = &preview.instance.desired.spec;
        let mut result = match_route(&spec.routing, &spec.execution, destination, flow);
        if let CvmRouteMatch::Route {
            route_index, route, ..
        } = &mut result
        {
            if let RouteTarget::Interface { interface_id } = &mut route.target {
                if let Some((draft, _)) = preview
                    .mounted_interfaces
                    .iter()
                    .find(|(_, real)| real.as_str() == interface_id.as_str())
                {
                    interface_id.clone_from(draft);
                }
            }
            if let Some(input) = input {
                // The preview normalizes the table, so map the winning rule back to its UI row.
                *route_index = input
                    .iter()
                    .position(|candidate| candidate == route)
                    .ok_or_else(|| {
                        StoreError::Unavailable("无法将预览路由对应到草稿，请刷新后重试".into())
                    })?;
            }
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    #[test]
    fn route_match_uses_runtime_precedence_and_preserves_input_row_indices() {
        let routing = serde_json::from_value(json!({
            "revision":0, "dns":[],
            "priorities":[
                {"interface_id":"nic-a","ipv4":10,"ipv6":20},
                {"interface_id":"nic-b","ipv4":20,"ipv6":10},
                {"chain_id":"chain-a","ipv4":30,"ipv6":30}
            ],
            "routes":[
                {"destination":"0.0.0.0/0","interface_id":"nic-a"},
                {"destination":"192.0.2.0/24","chain_id":"chain-a"},
                {"destination":"192.0.2.0/25","interface_id":"nic-b"},
                {"destination":"0.0.0.0/0","interface_id":"nic-b","service":{"protocol":"tcp","port_start":442,"port_end":443}},
                {"destination":"0.0.0.0/0","interface_id":"nic-a","service":{"protocol":"tcp","port_start":400,"port_end":500}},
                {"destination":"0.0.0.0/0","interface_id":"nic-a","service":{"protocol":"tcp","port_start":443,"port_end":443}},
                {"destination":"2001:db8::/32","interface_id":"nic-a","service":{"protocol":"udp","port_start":53,"port_end":53}},
                {"destination":"2001:db8::/32","interface_id":"nic-b","service":{"protocol":"udp","port_start":53,"port_end":54}}
            ]
        }))
        .unwrap();
        let cube = ExecutionTarget::Cube;
        let baremetal = ExecutionTarget::BareMetal {
            identity: HostIdentity {
                machine: "a".repeat(64),
                root_filesystem: "b".repeat(64),
            },
        };
        let match_at = |execution, address: &str, flow| {
            serde_json::to_value(match_route(
                &routing,
                execution,
                address.parse().unwrap(),
                flow,
            ))
            .unwrap()
        };
        let tcp = Some((L4Protocol::Tcp, 443));
        let udp = Some((L4Protocol::Udp, 443));
        assert_eq!(match_at(&cube, "192.0.2.1", tcp)["route_index"], 2); // prefix before L4
        let narrow = match_at(&cube, "203.0.113.1", tcp);
        assert_eq!(narrow["route_index"], 5); // narrower range at equal priority
        assert_eq!(
            match_at(&cube, "203.0.113.1", Some((L4Protocol::Tcp, 442)))["route_index"],
            4
        );
        assert_eq!(narrow["priority"], 10);
        assert_eq!(narrow["route"]["service"]["port_end"], 443);
        assert_eq!(match_at(&cube, "203.0.113.1", udp)["route_index"], 0); // explicit /0
        assert_eq!(
            match_at(&cube, "2001:db8::1", Some((L4Protocol::Udp, 54)))["route_index"],
            7
        );
        assert_eq!(
            match_at(&cube, "2001:db8::1", Some((L4Protocol::Udp, 53)))["priority"],
            10
        );
        assert_eq!(
            match_at(&cube, "2001:db8::1", tcp),
            json!({"kind":"default"})
        );
        let dropped = match_at(&cube, "192.0.2.200", None);
        assert_eq!(dropped["route_index"], 1);
        assert_eq!(dropped["blocked"], "chain_requires_tcp_udp");
        assert_eq!(match_at(&cube, "192.0.2.200", tcp)["blocked"], Value::Null);
        assert_eq!(match_at(&baremetal, "192.0.2.200", None)["route_index"], 0);
    }

    #[test]
    fn route_match_input_requires_ip_literals_and_a_valid_protocol_port_pair() {
        fn request(
            destination: &str,
            protocol: &str,
            port: Option<i32>,
        ) -> Result<CvmRouteMatchRequest> {
            Ok(serde_json::from_value(
                json!({"destination":destination,"protocol":protocol,"port":port}),
            )?)
        }
        for ip in ["example.test", "192.0.2.0/24", "fe80::1%eth0"] {
            assert!(request(ip, "other", None).is_err());
        }
        assert!(request("192.0.2.1", "icmp", None).is_err());
        assert!(request("192.0.2.1", "tcp", Some(65_536)).is_err());
        assert!(request("192.0.2.1", "tcp", Some(0))
            .unwrap()
            .flow()
            .is_err());
        assert!(request("192.0.2.1", "udp", None).unwrap().flow().is_err());
        assert!(request("192.0.2.1", "other", Some(5))
            .unwrap()
            .flow()
            .is_err());
        assert!(request("::ffff:192.0.2.1", "other", None)
            .unwrap()
            .flow()
            .is_err());
        assert_eq!(
            request("2001:db8::1", "udp", Some(65_535))
                .unwrap()
                .flow()
                .unwrap(),
            Some((L4Protocol::Udp, 65_535))
        );
    }
}

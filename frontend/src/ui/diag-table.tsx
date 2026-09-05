// 诊断面板。两处渲染共用该组件（顶栏气泡 `forge/shell.tsx`、概览页
// `panes/index.tsx`）——上一版本在两处分别实现，导致修改时遗漏其中一处，
// 表现为服务端返回 `warnings: 0` 而角标显示「2 警」。共用后不会再出现该类问题。
//
// 结构包含三部分：
//
// 一、表格。四列对齐（级别 / 诊断码 / 位置 / 说明），一行一条。展开的详情行
//     占据整个宽度，因此长消息不再被固定宽度的列压缩。
//
// 二、提示默认折叠，折叠状态按操作者存入 localStorage。折叠不等于不显示：全部为提示时
//     面板显示一句结论加一个入口，而不是空白——「没有错误和警告」与「没有任何诊断」
//     是两种状态，渲染为空面板与实际不符。
//
// 三、位置显示为名称。`app-hk-01.c2/sg-01->au-01` 中的四个 id 都不便于识别，
//     同一信息用名称表示为「港新线路 · 新加坡中转 → 澳洲落地」。转换在
//     `formatLocation` 中实现，无法查找到的 id 保持原样。

import { Fragment, useState } from 'react';
import { formatLocation, type DiagNames, type Diagnostic } from '../api';

// 折叠状态按操作者存储。每次打开面板都需要重新点击展开会增加操作成本；
// 而默认值为折叠——首次打开时不应直接显示大量无法判定的诊断信息。
const KEY = 'brocade.diag.show-info';
const readShowInfo = () => {
  try {
    return localStorage.getItem(KEY) === '1';
  } catch {
    /* 隐私模式下 localStorage 会抛出异常。默认折叠，避免一个偏好设置导致面板无法使用。 */
    return false;
  }
};
const writeShowInfo = (on: boolean) => {
  try {
    localStorage.setItem(KEY, on ? '1' : '0');
  } catch {
    /* 同上：写入失败时该设置只在本次会话内生效 */
  }
};

export function DiagTable({ diagnostics, names }: { diagnostics: Diagnostic[]; names: DiagNames }) {
  const [showInfo, setShowInfo] = useState(readShowInfo);
  const [open, setOpen] = useState<number | null>(null);

  const errors = diagnostics.filter(d => d.level === 'error').length;
  const warnings = diagnostics.filter(d => d.level === 'warn').length;
  const infos = diagnostics.filter(d => d.level === 'info').length;

  const toggleInfo = () => {
    const next = !showInfo;
    setShowInfo(next);
    writeShowInfo(next);
    setOpen(null);
  };

  /* 错误和警告无条件显示——此时的目的是处理问题，不应被大量提示干扰。 */
  const loud = diagnostics.filter(d => d.level !== 'info');
  const quiet = diagnostics.filter(d => d.level === 'info');
  const rows = showInfo ? [...loud, ...quiet] : loud;

  return (
    <>
      <div className="dg-h">
        诊断
        <span className="sp" />
        <span className={`dg-chip${errors ? ' on-err' : ''}`}>{errors} 错</span>
        <span className={`dg-chip${warnings ? ' on-warn' : ''}`}>{warnings} 警</span>
        {/* 没有任何提示时不渲染该控件：没有可展开的内容时，一个开关样式的元素
            会被理解为存在隐藏内容。 */}
        {infos > 0 && (
          <button
            className={`dg-chip tog${showInfo ? ' open' : ''}`}
            onClick={toggleInfo}
            title={showInfo ? '收起提示' : '显示提示'}
          >
            {infos} 提示 <span className="cv">{showInfo ? '▾' : '▸'}</span>
          </button>
        )}
      </div>

      {rows.length === 0 ? (
        <div className="dg-clean">
          <span className="big">
            {loud.length === 0 && infos === 0 ? '编译干净 · 可以发布' : '没有错误，也没有警告 · 可以发布'}
          </span>
          {infos > 0 && <>另有 {infos} 条提示：编译器判断不了实际风险的那些事实</>}
        </div>
      ) : (
        <div className="dg-b">
          <table className="dg-tbl">
            <thead>
              <tr>
                <th className="c-lv" />
                <th className="c-code">诊断码</th>
                <th className="c-at">位置</th>
                <th>说明</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((d, i) => {
                // 提示段前增加一条分隔带：不加时，提示行与上方的错误行相邻，
                // 级别差异只由圆点颜色表示，区分度不足。
                const bandHere = showInfo && d.level === 'info' && i === loud.length;
                const at = formatLocation(d.location, names);
                return (
                  <Fragment key={i}>
                    {bandHere && (
                      <tr className="dg-band">
                        <td colSpan={4}>
                          提示 · 编译器判断不了的事实，不挡发布<span className="sp">{infos}</span>
                        </td>
                      </tr>
                    )}
                    <tr
                      className={`dg-r ${d.level}`}
                      role="button"
                      tabIndex={0}
                      aria-expanded={open === i}
                      onClick={() => setOpen(open === i ? null : i)}
                      onKeyDown={event => {
                        if (event.key === 'Enter' || event.key === ' ') {
                          event.preventDefault();
                          setOpen(open === i ? null : i);
                        }
                      }}
                    >
                      <td className="c-lv">
                        <span className="dot" />
                      </td>
                      <td className="c-code" title={d.code}>
                        {d.code}
                      </td>
                      {/* 位置从左侧截断：这些字符串的前缀高度重复（同属一个 app 或链），
                          差异在尾部，从右侧截断会切掉唯一有区分度的部分。
                          title 中保留未转换的原始字符串，排查时需要的是 id 而非名称。 */}
                      <td className="c-at" title={d.location}>
                        {at}
                      </td>
                      <td className="c-msg">{d.message}</td>
                    </tr>
                    {open === i && (
                      <tr className="dg-det">
                        <td colSpan={4}>
                          <div className="full">{d.message}</div>
                          <div className="kv">
                            <b>位置</b>
                            <span>{at}</span>
                          </div>
                          {at !== d.location && (
                            <div className="kv">
                              <b>原串</b>
                              <span>{d.location}</span>
                            </div>
                          )}
                          <div className="kv">
                            <b>级别</b>
                            <span>
                              {d.level === 'error'
                                ? 'error · 挡住发布'
                                : d.level === 'warn'
                                  ? 'warn · 不挡发布'
                                  : 'info · 不挡发布'}
                            </span>
                          </div>
                        </td>
                      </tr>
                    )}
                  </Fragment>
                );
              })}
            </tbody>
          </table>
        </div>
      )}

      {infos > 0 && (
        <div className="dg-f">
          {showInfo ? (
            <span>
              <b>{infos}</b> 条提示 · 全部显示
            </span>
          ) : (
            <span>
              另有 <b>{infos}</b> 条提示
            </span>
          )}
          <span className="sp" />
          <button className="lnk" onClick={toggleInfo}>
            {showInfo ? '收起提示' : '显示'}
          </button>
        </div>
      )}
    </>
  );
}

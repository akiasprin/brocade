/* 全线稿图标库。16 网格、1.5px 描边、圆角线帽、currentColor——用色由使用处的 CSS 定，
   不随主色。两个出口：ListIcon 是列表页标题行的固定用法（16px、.list-ico 定色 --ink-2）；
   Icon 是通用件，尺寸和着色类由调用点给（顶栏导航 14px、产物/诊断 13px）。 */

const PATHS = {
  /* 终端主机：机器既是受管节点，也是可进入诊断与配置的操作目标。 */
  nodes: (
    <>
      <rect x="2" y="2.8" width="12" height="10.4" rx="2" />
      <path d="M4.5 6 L6.5 8 L4.5 10" />
      <path d="M8.4 10 H11.4" />
    </>
  ),
  /* 两个端点加一条路由 */
  chains: (
    <>
      <circle cx="3.3" cy="12.6" r="1.7" />
      <circle cx="12.7" cy="3.4" r="1.7" />
      <path d="M4.8 11.3 C 7.5 8.6, 8.5 7.4, 11.2 4.7" />
    </>
  ),
  /* 一段轴向管道：左侧椭圆是接入口，横向体积表示一项可复用的传输资源。
     不使用箭头——隧道未来既能作为出口，也能作为入口；箭头还会让资源图标被读成
     “退出”动作。侧视管道与「线路」的端点曲线、机器的终端框都有独立轮廓。 */
  tunnels: (
    <>
      <ellipse cx="4.2" cy="8" rx="2.1" ry="4.3" />
      <path d="M4.2 3.7 H11.5 C12.8 3.7 13.8 5.6 13.8 8 C13.8 10.4 12.8 12.3 11.5 12.3 H4.2" />
    </>
  ),
  /* 头与肩 */
  users: (
    <>
      <circle cx="8" cy="5.2" r="2.6" />
      <path d="M3.2 13.4 C 3.2 10.6, 5.3 8.9, 8 8.9 C 10.7 8.9, 12.8 10.6, 12.8 13.4" />
    </>
  ),
  /* 放大镜：列表页的即时筛选。 */
  search: (
    <>
      <circle cx="7" cy="7" r="4.2" />
      <path d="M10.2 10.2 L13.8 13.8" />
    </>
  ),
  /* 出盘上箭头：发布 */
  deploy: (
    <>
      <path d="M8 2.3 V9.8" />
      <path d="M4.8 5.4 L8 2.2 L11.2 5.4" />
      <path d="M2.6 10.4 V12.6 A1.4 1.4 0 0 0 4 14 H12 A1.4 1.4 0 0 0 13.4 12.6 V10.4" />
    </>
  ),
  /* L 形坐标轴加三根柱：用量。带轴才不读作「山」 */
  usage: (
    <>
      <path d="M2.4 2.2 V13.6 H13.8" />
      <path d="M5.4 13.6 V9.4" />
      <path d="M8.6 13.6 V5.2" />
      <path d="M11.8 13.6 V7.4" />
    </>
  ),
  /* 齿轮：设置 */
  settings: (
    <>
      <circle cx="8" cy="8" r="2.3" />
      <path d="M8 1.9 V3.5" />
      <path d="M8 12.5 V14.1" />
      <path d="M1.9 8 H3.5" />
      <path d="M12.5 8 H14.1" />
      <path d="M4.2 4.2 L5.3 5.3" />
      <path d="M10.7 10.7 L11.8 11.8" />
      <path d="M11.8 4.2 L10.7 5.3" />
      <path d="M5.3 10.7 L4.2 11.8" />
    </>
  ),
  /* 立方体：产物 */
  artifacts: (
    <>
      <path d="M8 1.9 L13.8 4.9 V11.1 L8 14.1 L2.2 11.1 V4.9 Z" />
      <path d="M2.2 4.9 L8 7.9 L13.8 4.9" />
      <path d="M8 7.9 V14.1" />
    </>
  ),
  /* 脉搏线：诊断 */
  diag: <path d="M1.8 8 H4.6 L6.4 3.4 L9.6 12.6 L11.4 8 H14.2" />,
  /* 趋势线：观测（机器详情页签，指标上行 + 端点） */
  observe: (
    <>
      <path d="M2.3 11.2 L 6 7.4 L 8.8 9.4 L 13.7 4.3" />
      <circle cx="13.7" cy="4.3" r="0.95" fill="currentColor" stroke="none" />
    </>
  ),
  /* 滑杆：配置（机器详情页签，与全局设置的齿轮区分） */
  config: (
    <>
      <path d="M3 5.6 H13" />
      <circle cx="6" cy="5.6" r="1.7" />
      <path d="M3 10.4 H13" />
      <circle cx="10" cy="10.4" r="1.7" />
    </>
  ),
  /* 一下一上两支箭：agent 的两条通道——拉配置、报用量。
     不用循环箭头——那表示重试，而这里是双向传输。 */
  agent: (
    <>
      <path d="M5.2 3.2 V12.8" />
      <path d="M2.6 10.2 L5.2 12.8 L7.8 10.2" />
      <path d="M10.8 12.8 V3.2" />
      <path d="M8.2 5.8 L10.8 3.2 L13.4 5.8" />
    </>
  ),
  /* 产物状态：已应用 / 已关闭 / 已变化。
     关闭用一条横线而不是叉——叉读作失败，而 disabled 是一个正常取值，需要与 dirty 区分。
     变化用 ≠，即「机器上的与该修订不一致」。 */
  check: <path d="M3.4 8.4 L6.6 11.6 L12.8 4.8" />,
  dash: <path d="M3.8 8 H12.2" />,
  neq: (
    <>
      <path d="M3.4 6.3 H12.6" />
      <path d="M3.4 9.7 H12.6" />
      <path d="M10.3 3.2 L5.7 12.8" />
    </>
  ),
  /* 异常读数标记。判定结果此前只由金色文字表达，颜色之外再给一个形状，
     使异常项在一片读数里可定位。 */
  warn: (
    <>
      <path d="M8 2.4 L14.6 13.6 H1.4 Z" />
      <path d="M8 6.6 V9.7" />
      <circle cx="8" cy="11.7" r="0.55" fill="currentColor" stroke="none" />
    </>
  ),
};

export type IconName = keyof typeof PATHS;

export function Icon({ of, size = 16, className }: { of: IconName; size?: number; className?: string }) {
  return (
    <span className={className} aria-hidden="true">
      <svg
        width={size}
        height={size}
        viewBox="0 0 16 16"
        fill="none"
        stroke="currentColor"
        strokeWidth="1.5"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        {PATHS[of]}
      </svg>
    </span>
  );
}

/* 列表页标题行的固定用法：16px，颜色由 .list-ico 定为 --ink-2，比 15px/600 的标题浅一档。 */
export function ListIcon({ of }: { of: 'nodes' | 'chains' | 'tunnels' | 'users' }) {
  return <Icon of={of} size={16} className="list-ico" />;
}

import type { ReactNode } from 'react';

/* 图标库以线稿为主，品牌标识保留原轮廓。16 网格、1.5px 描边、圆角线帽，
   currentColor 由使用处的 CSS 定。ListIcon 与 PanelTitle 共用 14px 标题图标；
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
  /* 两层机架：外部服务器（如 VPN Gate 目录节点），与表示受管机器的终端框 nodes 区分。 */
  servers: (
    <>
      <rect x="2.2" y="2.3" width="11.6" height="4.7" rx="1.2" />
      <rect x="2.2" y="9" width="11.6" height="4.7" rx="1.2" />
      <circle cx="4.9" cy="4.65" r="0.8" fill="currentColor" stroke="none" />
      <circle cx="4.9" cy="11.35" r="0.8" fill="currentColor" stroke="none" />
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
  /* 更多菜单里的拓扑入口：三点分支比完整线路曲线更轻，也直接表达节点关系。 */
  topology: (
    <>
      <path d="M8 7.3 V4.3 M7.3 8.3 L4.4 11.1 M8.7 8.3 L11.6 11.1" strokeWidth="1.25" />
      <circle cx="8" cy="3.2" r="1.15" fill="currentColor" stroke="none" />
      <circle cx="3.5" cy="12" r="1.15" fill="currentColor" stroke="none" />
      <circle cx="12.5" cy="12" r="1.15" fill="currentColor" stroke="none" />
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
  /* 身份卡：用于机器自身的名称、归属与公网地址。 */
  identity: (
    <>
      <rect x="1.8" y="3" width="12.4" height="10" rx="1.6" />
      <circle cx="5.2" cy="7" r="1.5" />
      <path d="M2.9 11 C3.3 9.6 4.1 8.9 5.2 8.9 C6.3 8.9 7.1 9.6 7.5 11" />
      <path d="M9.2 6 H12.2 M9.2 9 H12.2" />
    </>
  ),
  /* 带折角和印记的证书。 */
  certificate: (
    <>
      <path d="M3 1.8 H10 L13 4.8 V14.2 H3 Z" />
      <path d="M10 1.8 V4.8 H13" />
      <circle cx="8" cy="9.2" r="2.1" />
      <path d="M6.8 9.2 L7.6 10 L9.3 8.3" />
    </>
  ),
  /* DNS：经纬线构成的解析域。 */
  dns: (
    <>
      <circle cx="8" cy="8" r="6" />
      <path d="M2.3 8 H13.7 M8 2 C10.8 5.1 10.8 10.9 8 14 M8 2 C5.2 5.1 5.2 10.9 8 14" />
    </>
  ),
  /* 旗帜：地区。不用地球轮廓，地球已表示 DNS。 */
  region: (
    <>
      <path d="M3.2 14.2 V2.6" />
      <path d="M3.2 2.6 C4.6 1.8 5.9 1.8 7.4 2.5 C8.9 3.2 10.4 3.4 12.8 2.6 V8.9 C10.4 9.7 8.9 9.5 7.4 8.8 C5.9 8.1 4.6 8.1 3.2 8.9" />
    </>
  ),
  /* 安全边界。 */
  security: (
    <>
      <path d="M8 1.8 C9.7 3 11.6 3.5 13.5 3.7 V7.6 C13.5 10.6 11.5 12.8 8 14.2 C4.5 12.8 2.5 10.6 2.5 7.6 V3.7 C4.4 3.5 6.3 3 8 1.8 Z" />
      <path d="M5.5 8 L7.2 9.7 L10.8 6.1" />
    </>
  ),
  /* Project X 官网标识：https://xtls.github.io/logo-light.svg
     来源 Project X 社区（网站 CC-BY-SA 4.0）；保留原轮廓，缩放到 16 网格并跟随文字颜色。 */
  xray: (
    <g transform="scale(0.016)" fill="currentColor" stroke="none">
      <polygon points="530,530 900,530 650,650 530,1000" />
      <polygon points="470,530 470,900 350,650 0,530" />
      <polygon points="530,470 530,100 650,350 1000,470" />
      <polygon points="470,470 100,470 350,350 470,0" />
    </g>
  ),
  /* Hysteria 官网标识：https://v2.hysteria.network/assets/logo.svg
     保留原轮廓和黄色部分，中性色随文字颜色适配主题。 */
  hysteria: (
    <g transform="translate(0 2.15) scale(0.03618)" fill="currentColor" stroke="none">
      <polygon points="72.8 140.45 60.1 285.55 102.51 285.55 111.5 182.86 111.54 182.86 115.21 140.45 72.8 140.45" />
      <polygon points="124.16 37.75 81.76 37.75 76.19 101.36 118.59 101.36 124.16 37.75" />
      <polygon points="318.36 285.56 360.44 285.56 366.01 221.95 323.9 221.95 318.36 285.56" />
      <polygon points="382.09 37.76 340 37.76 329.16 161.66 221.09 161.66 206.95 323.31 292.78 201.84 438.67 201.84 442.19 161.66 371.25 161.66 382.09 37.76" />
      <polygon fill="#ffbc00" points="149.41 121.47 3.52 121.47 0 161.66 221.09 161.66 235.23 0 149.41 121.47" />
    </g>
  ),
  /* AnyTLS 使用闪电。 */
  bolt: <path d="M9.2 1.8 L3.2 8.8 H7 L6.8 14.2 L12.8 7.2 H9 Z" />,
  /* 箭头进入监听边界。 */
  ingress: (
    <>
      <path d="M1.8 8 H9.3 M6.2 4.9 L9.3 8 L6.2 11.1" />
      <path d="M10.5 2.8 H14 V13.2 H10.5" />
    </>
  ),
  /* 客户端屏幕。 */
  client: (
    <>
      <rect x="2" y="2.5" width="12" height="8.5" rx="1.4" />
      <path d="M8 11 V13.5 M5.2 13.5 H10.8" />
    </>
  ),
  /* 协议栈：三层相叠。 */
  protocol: (
    <>
      <path d="M2.2 5 L8 2 L13.8 5 L8 8 Z" />
      <path d="M2.2 8 L8 11 L13.8 8 M2.2 11 L8 14 L13.8 11" />
    </>
  ),
  /* 授权密钥。 */
  access: (
    <>
      <circle cx="5.1" cy="8" r="3.1" />
      <path d="M8.2 8 H14 M11 8 V10.2 M13 8 V9.5" />
    </>
  ),
  /* 从边界向外发起连接。 */
  outbound: (
    <>
      <path d="M14.2 8 H6.7 M9.8 4.9 L6.7 8 L9.8 11.1" />
      <path d="M5.5 2.8 H2 V13.2 H5.5" />
    </>
  ),
  /* 放大镜：列表页的即时筛选。 */
  search: (
    <>
      <circle cx="7" cy="7" r="4.2" />
      <path d="M10.2 10.2 L13.8 13.8" />
    </>
  ),
  /* 单条可复制地址：两节相扣，区别于表示完整线路拓扑的 chains。 */
  link: (
    <>
      <path d="M6.4 10.7 L5.2 11.9 A2.6 2.6 0 0 1 1.5 8.2 L4.2 5.5 A2.6 2.6 0 0 1 7.9 5.5" />
      <path d="M9.6 5.3 L10.8 4.1 A2.6 2.6 0 0 1 14.5 7.8 L11.8 10.5 A2.6 2.6 0 0 1 8.1 10.5" />
      <path d="M5.7 8 H10.3" />
    </>
  ),
  /* 订阅源：圆点和两级扩散弧沿用 feed 的通用语义。 */
  subscription: (
    <>
      <circle cx="3.5" cy="12.5" r="1" fill="currentColor" stroke="none" />
      <path d="M3 7.5 A5.5 5.5 0 0 1 8.5 13" />
      <path d="M3 3 A10 10 0 0 1 13 13" />
    </>
  ),
  /* 顶栏功能菜单：短而细的错落横线，保持菜单语义，同时不在紧凑按钮里形成粗重色块。 */
  menu: (
    <>
      <path d="M3.5 4.5 H12.5" />
      <path d="M5.2 8 H12.5" />
      <path d="M3.5 11.5 H10.8" />
    </>
  ),
  /* 带文字的更多操作按钮使用横向三点，不复用纯字符以保持描边和基线一致。 */
  more: (
    <>
      <circle cx="3.2" cy="8" r="1" fill="currentColor" stroke="none" />
      <circle cx="8" cy="8" r="1" fill="currentColor" stroke="none" />
      <circle cx="12.8" cy="8" r="1" fill="currentColor" stroke="none" />
    </>
  ),
  /* 日期范围：标题栏观测时间选择器。 */
  calendar: (
    <>
      <rect x="2.2" y="3.4" width="11.6" height="10.3" rx="1.7" />
      <path d="M5 1.9 V4.8 M11 1.9 V4.8 M2.2 6.4 H13.8" />
      <path d="M5 8.8 H6 M8 8.8 H9 M11 8.8 H12 M5 11.2 H6 M8 11.2 H9" />
    </>
  ),
  /* 亮暗与配色：半明半暗的太阳。 */
  theme: (
    <>
      <circle cx="8" cy="8" r="3.1" />
      <path d="M8 1.4 V3 M8 13 V14.6 M1.4 8 H3 M13 8 H14.6 M3.3 3.3 L4.4 4.4 M11.6 11.6 L12.7 12.7 M12.7 3.3 L11.6 4.4 M4.4 11.6 L3.3 12.7" />
      <path d="M8 4.9 A3.1 3.1 0 0 0 8 11.1 Z" fill="currentColor" stroke="none" opacity=".22" />
    </>
  ),
  /* 外观分段项使用独立的日/月轮廓，当前模式无需再从一枚混合图标里猜。 */
  sun: (
    <>
      <circle cx="8" cy="8" r="2.7" />
      <path d="M8 1.5 V3 M8 13 V14.5 M1.5 8 H3 M13 8 H14.5 M3.4 3.4 L4.45 4.45 M11.55 11.55 L12.6 12.6 M12.6 3.4 L11.55 4.45 M4.45 11.55 L3.4 12.6" />
    </>
  ),
  moon: <path d="M12.8 10.6 A5.6 5.6 0 0 1 5.4 3.2 A5.8 5.8 0 1 0 12.8 10.6 Z" />,
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
  /* 顶栏产物开关：带页签的文件夹，表达可展开浏览的一组生成结果。 */
  artifactFolder: (
    <>
      <path d="M2.2 5 V4.2 C2.2 3.4 2.8 2.8 3.6 2.8 H6.4 L7.9 4.5 H12.4 C13.2 4.5 13.8 5.1 13.8 5.9 V12.1 C13.8 12.9 13.2 13.5 12.4 13.5 H3.6 C2.8 13.5 2.2 12.9 2.2 12.1 Z" />
      <path d="M2.3 6.2 H13.7" />
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
  /* 面包屑流量读数：箭头指向机器表示接收，离开机器表示发送。 */
  rx: (
    <>
      <path d="M8 3 V12.3" />
      <path d="M4.4 8.9 L8 12.5 L11.6 8.9" />
    </>
  ),
  tx: (
    <>
      <path d="M8 13 V3.7" />
      <path d="M4.4 7.3 L8 3.7 L11.6 7.3" />
    </>
  ),
  /* 发布页的「可升级」：机器落后于可发版本，等待批准。与 tx 同形，按语义单列。 */
  upgrade: (
    <>
      <path d="M8 12.8 V3.6" />
      <path d="M4.6 7 L8 3.6 L11.4 7" />
    </>
  ),
  /* 产物状态：已应用 / 已关闭 / 已变化。
     关闭用一条横线而不是叉——叉读作失败，而 disabled 是一个正常取值，需要与 dirty 区分。
     变化用 ≠，即「机器上的与该修订不一致」。 */
  check: <path d="M3.4 8.4 L6.6 11.6 L12.8 4.8" />,
  copy: (
    <>
      <rect x="5" y="5" width="8" height="8" rx="1.5" />
      <path d="M11 5V3.5A1.5 1.5 0 0 0 9.5 2h-6A1.5 1.5 0 0 0 2 3.5v6A1.5 1.5 0 0 0 3.5 11H5" />
    </>
  ),
  close: <path d="M3.5 3.5 L12.5 12.5 M12.5 3.5 L3.5 12.5" />,
  /* 展开符：顶栏账户牌表示点击后展开菜单。 */
  chevronDown: <path d="M4.6 6.4 L8 9.8 L11.4 6.4" />,
  dash: <path d="M3.8 8 H12.2" />,
  /* 在等：发布流水里表示等待确认或待补偿。与 check / close 同为结果图标，因此同样只有线稿。 */
  clock: (
    <>
      <circle cx="8" cy="8" r="5.6" />
      <path d="M8 5 V8.2 L10.2 9.6" />
    </>
  ),
  neq: (
    <>
      <path d="M3.4 6.3 H12.6" />
      <path d="M3.4 9.7 H12.6" />
      <path d="M10.3 3.2 L5.7 12.8" />
    </>
  ),
  /* 圆中的 i：只读的说明信息。 */
  info: (
    <>
      <circle cx="8" cy="8" r="6" />
      <path d="M8 7.3 V11.3" />
      <circle cx="8" cy="4.9" r="0.85" fill="currentColor" stroke="none" />
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
  /* 垃圾桶：永久移除已经退役的资源。只用于明确的破坏性操作，不与普通的减号混用。 */
  trash: (
    <>
      <path d="M2.5 4.5 H13.5" />
      <path d="M5.7 4.5 V2.5 H10.3 V4.5" />
      <path d="M4.1 4.5 L4.9 13.5 H11.1 L11.9 4.5" />
      <path d="M6.6 7 V11 M9.4 7 V11" />
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

const PANEL_TITLE_ICON_SIZE = 14;

/* 列表标题的独立图标，与 PanelTitle 使用同一尺寸。 */
export function ListIcon({ of }: { of: IconName }) {
  return <Icon of={of} size={PANEL_TITLE_ICON_SIZE} className="list-ico" />;
}

/* 详情页卡片标题的统一骨架。图标放进 h4 内部，使标题与右侧状态、按钮仍是 header 的
   独立 flex 项；如果把图标直接放在 header 下，标题和图标会被当作两列，窄屏换行时分开。 */
export function PanelTitle({ of, children }: { of: IconName; children: ReactNode }) {
  return (
    <h4 className="panel-title">
      <Icon of={of} size={PANEL_TITLE_ICON_SIZE} className="panel-title-icon" />
      {children}
    </h4>
  );
}

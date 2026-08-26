import { createContext, useContext, type ReactNode } from 'react';
import { loginAdmin, type AdminRole, type Whoami } from './api';

export interface Session {
  who: Whoami;
}

const Ctx = createContext<Session | null>(null);

export function SessionProvider({ value, children }: { value: Session; children: ReactNode }) {
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function useSession(): Session {
  const s = useContext(Ctx);
  if (!s) throw new Error('useSession 只能在登录之后用');
  return s;
}

/* 免密的公开账户。id 是约定值：服务端使用同一个 id（PUBLIC_OPERATOR_ID），
   两侧必须一致。 */
export const PUBLIC_ID = 'public';

/** 表示未登录的公开访客，而非已登录的操作者。顶栏只为其保留「机器」和「项目」。 */
export const isPublic = (who: Whoami) => who.operator_id === PUBLIC_ID;

/* 该标签页中已执行退出的标记：设置后不再自动登录为 public，否则退出后的下一帧
   会回到访客视角，无法显示登录表单。使用 sessionStorage 而非 localStorage：
   关闭标签页后应清除，否则一次退出会使该设备之后不再自动进入。 */
const NO_AUTO_PUBLIC = 'brocade.no-auto-public';

export const autoPublicSuppressed = () => !!sessionStorage.getItem(NO_AUTO_PUBLIC);
export const suppressAutoPublic = () => sessionStorage.setItem(NO_AUTO_PUBLIC, '1');

/**
 * 以公开访客身份登录。手动触发（登录页的「访客模式」）和自动登录使用同一路径。
 *
 * 同时清除上述标记：显式点击「访客模式」表示需要进入访客视角，刷新后应保持该状态——
 * 否则该按钮的效果在刷新后失效。
 */
export async function enterPublic() {
  sessionStorage.removeItem(NO_AUTO_PUBLIC);
  // 免密账户使用空密码登录（服务端 authenticate_admin_password）。
  const result = await loginAdmin({ operator_id: PUBLIC_ID, password: '' });
  return result.admin;
}

const RANK: Record<AdminRole, number> = {
  readonly: 0,
  editor: 1,
  publisher: 2,
  'tenant-admin': 3,
  'system-admin': 4,
};

// 按角色禁用控件属于体验优化而非安全边界：服务端的每个写接口都会再次鉴权。
// 界面一律禁用而不隐藏：同一个台面上各页的标题栏和操作区在不同身份下应当是同一副形状，
// 少掉一枚按钮会被读成这一页不支持那个动作。原因不写在界面上——`title` 里挂一句
// 「要 xxx-admin」等于把角色模型摊给每个只读访客，而禁用本身已经说明了当前状态。
// tenant-admin 在 API 侧同时具备 edit 和 publish 权限，因此使用序号比较而非集合。
export function can(
  role: AdminRole,
  need: 'read' | 'artifacts' | 'edit' | 'publish' | 'manage-tenants' | 'system',
): boolean {
  switch (need) {
    case 'read':
      return true;
    /* 产物是完整可用的配置：地址出现在几乎每一行，掩码后不再是可用的配置，
       因此评审角色（readonly）在 API 侧直接返回 403。此处据此禁用对应入口
       （顶栏的「产物」、用户行的 VLESS / Clash），避免点击后只得到错误。 */
    case 'artifacts':
      return RANK[role] >= RANK.editor;
    case 'edit':
      return RANK[role] >= RANK.editor;
    case 'publish':
      return RANK[role] >= RANK.publisher;
    /* 创建租户和管理操作者使用同一权限：API 侧的 ManageOperators */
    case 'manage-tenants':
      return role === 'tenant-admin' || role === 'system-admin';
    case 'system':
      return role === 'system-admin';
  }
}

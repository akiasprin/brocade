import type { CSSProperties } from 'react';

export interface AvatarLamp {
  tone: '' | 'bad' | 'idle';
  title: string;
}

export const USER_AVATAR_MARKS = [
  'strawberry',
  'sprout',
  'citrus',
  'hourglass',
  'cherry',
  'grape',
  'pear',
  'leaf',
  'camera',
  'cactus',
  'key',
  'umbrella',
  'compass',
  'apple',
  'flower',
  'book',
  'clover',
  'mushroom',
  'bell',
  'paper-plane',
  'cup',
  'lantern',
  'feather',
  'sailboat',
] as const;

export type UserAvatarMark = (typeof USER_AVATAR_MARKS)[number];

export interface UserAvatarIdentity {
  mark: UserAvatarMark;
  hue: number;
}

type UserAvatarStyle = CSSProperties & {
  '--avatar-hue': number;
};

// FNV-1a 只用作稳定的视觉种子，不承担任何安全用途。同一用户在所有视图、设备和会话中
// 都会映射到同一套轮廓与低饱和配色；无需在数据库中保存头像，也不会在 render 时重新随机。
const hashUserId = (id: string) => {
  let hash = 0x811c9dc5;
  for (const char of id) {
    hash ^= char.codePointAt(0) ?? 0;
    hash = Math.imul(hash, 0x01000193) >>> 0;
  }
  return hash;
};

export const userAvatarIdentity = (id: string): UserAvatarIdentity => {
  const hash = hashUserId(id);
  const mixedHash = (hash ^ (hash >>> 16)) >>> 0;
  return {
    mark: USER_AVATAR_MARKS[mixedHash % USER_AVATAR_MARKS.length],
    // 量化为 24 档：足以区分相邻用户，又不会制造大量肉眼难辨的近似色。
    hue: ((hash >>> 8) % 24) * 15,
  };
};

function AvatarMarkPaths({ mark }: { mark: UserAvatarMark }) {
  switch (mark) {
    case 'apple':
      return (
        <>
          <path d="M12 7.2c-1.6 0-2.7-1.1-4.4-1.1-3.1 0-4.5 2.6-4.1 5.9.6 5.1 3.5 8.5 5.6 8.5 1.2 0 1.8-.7 2.9-.7s1.7.7 2.9.7c2.1 0 5-3.4 5.6-8.5.4-3.3-1-5.9-4.1-5.9-1.7 0-2.8 1.1-4.4 1.1Z" />
          <path d="M12.2 6.2c-.1-2.1.9-3.8 3-4.7M13.6 4.2c2 .2 3.5-.5 4.3-2-2-.5-3.6.2-4.3 2Z" />
        </>
      );
    case 'cherry':
      return (
        <>
          <circle cx="7.6" cy="16.1" r="3.4" />
          <circle cx="16.4" cy="16.1" r="3.4" />
          <path d="M7.7 12.7c.2-4 2.1-7.2 5.2-8.8M16.3 12.7c-.5-3.5-1.6-6.4-3.4-8.8M12.9 3.9c2.3-.9 4.4-.5 5.8 1.1-2.1 1-4.2.6-5.8-1.1Z" />
        </>
      );
    case 'citrus':
      return (
        <>
          <circle cx="12" cy="13.1" r="7.8" />
          <path d="M12.8 5.2c.2-1.7 1.2-2.9 2.9-3.7M14.5 3.5c2-.2 3.5.5 4.3 2-2 .5-3.6-.2-4.3-2Z" />
        </>
      );
    case 'grape':
      return (
        <>
          <circle cx="9" cy="9.2" r="2.3" />
          <circle cx="14.4" cy="9.2" r="2.3" />
          <circle cx="6.6" cy="13.7" r="2.3" />
          <circle cx="12" cy="13.7" r="2.3" />
          <circle cx="17.4" cy="13.7" r="2.3" />
          <circle cx="9.3" cy="18.1" r="2.3" />
          <circle cx="14.7" cy="18.1" r="2.3" />
          <circle cx="12" cy="21" r="1.7" />
          <path d="M11.8 6.9c0-2.2.8-3.8 2.5-4.9M13.4 4.1c2-.5 3.7 0 4.8 1.5-2 .8-3.7.2-4.8-1.5Z" />
        </>
      );
    case 'pear':
      return (
        <>
          <path d="M12 4.7c-1.2 0-2 1.2-2.5 3-.5 1.7-1.4 2.6-2.6 3.8-1.3 1.3-2 3-2 4.8 0 3.4 2.9 5.7 7.1 5.7s7.1-2.3 7.1-5.7c0-1.8-.7-3.5-2-4.8-1.2-1.2-2.1-2.1-2.6-3.8-.5-1.8-1.3-3-2.5-3Z" />
          <path d="M12.3 4.7c.2-1.6 1.1-2.8 2.7-3.5M14 3.2c1.8-.2 3.2.4 4 1.8-1.8.5-3.3-.1-4-1.8Z" />
        </>
      );
    case 'strawberry':
      return (
        <>
          <path d="M4.8 9.2c.3-2.4 2.8-3.8 7.2-3.8s6.9 1.4 7.2 3.8C19.7 13 16.6 18.6 12 22 7.4 18.6 4.3 13 4.8 9.2Z" />
          <path d="m7.3 6.5-1.2-3 3.4 1L12 2l2.5 2.5 3.4-1-1.2 3M9 10.2h.1M14.9 10.2h.1M12 14h.1M9.2 16.7h.1M14.8 16.7h.1" />
        </>
      );
    case 'sprout':
      return (
        <>
          <path d="M12 20V9.5M12 13.5c-4.1.1-6.8-2.2-7.2-6.1 4-.2 6.8 1.9 7.2 6.1ZM12 10.8c.5-3.7 3.2-5.8 7.1-5.7-.2 3.8-2.9 6-7.1 5.7Z" />
          <path d="M7.8 20h8.4" />
        </>
      );
    case 'leaf':
      return (
        <>
          <path d="M20.4 3.6C12.7 4 6.1 6.7 4.4 12.1c-1.2 3.8.9 7 4.5 7.2 5.7.4 9.7-5.9 11.5-15.7Z" />
          <path d="M5.5 20.1c2.7-4.8 6.4-8.8 11.1-12.1M9.8 14.4l-.6-4M13 11.2l3.8.2" />
        </>
      );
    case 'flower':
      return (
        <>
          <circle cx="12" cy="12" r="2.1" />
          <path d="M12 9.9c-2.5-.8-3.5-2.4-2.9-4.2.5-1.5 2-2.3 2.9-2.8.9.5 2.4 1.3 2.9 2.8.6 1.8-.4 3.4-2.9 4.2ZM14 11.4c.8-2.5 2.4-3.5 4.2-2.9 1.5.5 2.3 2 2.8 2.9-.5.9-1.3 2.4-2.8 2.9-1.8.6-3.4-.4-4.2-2.9ZM13.3 13.7c2.1 1.5 2.5 3.4 1.4 4.9-.9 1.3-2.6 1.6-3.6 1.8-.6-.8-1.7-2.1-1.6-3.7.1-1.9 1.5-3.1 3.8-3ZM10.4 13.3c-1.5 2.1-3.4 2.5-4.9 1.4-1.3-.9-1.6-2.6-1.8-3.6.8-.6 2.1-1.7 3.7-1.6 1.9.1 3.1 1.5 3 3.8Z" />
        </>
      );
    case 'clover':
      return (
        <>
          <path d="M11.8 12.2C8.5 11.6 6 9.8 6 7.3 6 5.5 7.3 4 9 4c1.5 0 2.5 1 3 2.3C12.5 5 13.5 4 15 4c1.7 0 3 1.5 3 3.3 0 2.5-2.5 4.3-5.8 4.9 3.3.6 5.8 2.4 5.8 4.9 0 1.8-1.3 3.3-3 3.3-1.5 0-2.5-1-3-2.3-.5 1.3-1.5 2.3-3 2.3-1.7 0-3-1.5-3-3.3 0-2.5 2.5-4.3 5.8-4.9Z" />
          <path d="M12 12.2v9" />
        </>
      );
    case 'cactus':
      return (
        <>
          <path d="M9.2 19V7.4a2.8 2.8 0 0 1 5.6 0V19M9.2 13H7.4a2.4 2.4 0 0 1-2.4-2.4V8.8M14.8 15h1.8a2.4 2.4 0 0 0 2.4-2.4v-2" />
          <path d="M7 19h10l-1 3H8l-1-3Z" />
        </>
      );
    case 'mushroom':
      return (
        <>
          <path d="M3.6 11.4C4.1 6.6 7.4 3.6 12 3.6s7.9 3 8.4 7.8c-4.9 1.2-11.9 1.2-16.8 0Z" />
          <path d="M9.1 12.2 8 20.4c2.5.8 5.5.8 8 0l-1.1-8.2M8.2 8.2h.1M15.8 7h.1" />
        </>
      );
    case 'key':
      return (
        <>
          <circle cx="8" cy="8.2" r="4.1" />
          <path d="m10.9 11.1 9 9M15.3 15.5l2.2-2.2M17.8 18l2.2-2.2M6.6 8.2h.1" />
        </>
      );
    case 'camera':
      return (
        <>
          <path d="M4 7.8h3l1.4-2.2h7.2L17 7.8h3a1.5 1.5 0 0 1 1.5 1.5v9.2A1.5 1.5 0 0 1 20 20H4a1.5 1.5 0 0 1-1.5-1.5V9.3A1.5 1.5 0 0 1 4 7.8Z" />
          <circle cx="12" cy="13.8" r="4" />
          <path d="M18.1 10.6h.1" />
        </>
      );
    case 'umbrella':
      return (
        <>
          <path d="M3 12.5a9 9 0 0 1 18 0c-1.6-1.4-3.2-1.4-4.8 0-1.4-1.4-2.8-1.4-4.2 0-1.4-1.4-2.8-1.4-4.2 0-1.6-1.4-3.2-1.4-4.8 0ZM12 3.5V19a2.5 2.5 0 0 0 5 0" />
        </>
      );
    case 'bell':
      return (
        <>
          <path d="M5 17.5h14l-1.8-2.3V10a5.2 5.2 0 0 0-10.4 0v5.2L5 17.5Z" />
          <path d="M9.5 19.2a2.7 2.7 0 0 0 5 0M10.7 3.2a1.4 1.4 0 0 1 2.6 0" />
        </>
      );
    case 'compass':
      return (
        <>
          <circle cx="12" cy="12" r="9" />
          <path d="m15.6 8.4-2.1 5.1-5.1 2.1 2.1-5.1 5.1-2.1Z" />
          <path d="M12 3v1M12 20v1M3 12h1M20 12h1" />
        </>
      );
    case 'book':
      return (
        <>
          <path d="M3.5 5.1c3.6-.8 6.4 0 8.5 2.3v12.1c-2.1-2.3-4.9-3.1-8.5-2.3V5.1ZM20.5 5.1c-3.6-.8-6.4 0-8.5 2.3v12.1c2.1-2.3 4.9-3.1 8.5-2.3V5.1Z" />
        </>
      );
    case 'paper-plane':
      return (
        <>
          <path d="m3 11 18-8-6.2 18-3.6-7-8.2-3Z" />
          <path d="m11.2 14 4.2-5.2" />
        </>
      );
    case 'cup':
      return (
        <>
          <path d="M5 8.5h12v7.2A4.3 4.3 0 0 1 12.7 20H9.3A4.3 4.3 0 0 1 5 15.7V8.5ZM17 10h1.2a2.8 2.8 0 1 1 0 5.6H17" />
          <path d="M8 5.8c-1-1.2.8-1.8 0-3M12 5.8c-1-1.2.8-1.8 0-3" />
        </>
      );
    case 'hourglass':
      return (
        <>
          <path d="M6 3h12M6 21h12M7 3c0 4 1.8 6.2 5 9-3.2 2.8-5 5-5 9M17 3c0 4-1.8 6.2-5 9 3.2 2.8 5 5 5 9" />
          <path d="M8.8 6.5h6.4M8.7 18.2c2.2-2 4.4-2 6.6 0" />
        </>
      );
    case 'lantern':
      return (
        <>
          <path d="M9 4V2h6v2M7 7h10l1.5 3v8L17 21H7l-1.5-3v-8L7 7Z" />
          <path d="M8.2 10h7.6M8.2 18h7.6M12 10v8" />
        </>
      );
    case 'feather':
      return (
        <>
          <path d="M5 20c5.5-1 11.7-6.5 14.5-15.8-7.2-.8-13 3-14.2 8.4-.5 2.3.1 4.1 1.7 5.3" />
          <path d="M4 21 16.5 7.5M8 17.1l-.2-4.5M11.5 13.7l-.1-4M11.7 12.7l4.2-.6M8.2 16.2l4.4-.2" />
        </>
      );
    case 'sailboat':
      return (
        <>
          <path d="M12 3v12M11.6 4 5 13h6.6M13 6l5.8 7H13M4 15h16l-2.5 4H7L4 15Z" />
          <path d="M3 21c2-1.2 4-1.2 6 0 2-1.2 4-1.2 6 0 2-1.2 4-1.2 6 0" />
        </>
      );
  }
}

function AvatarOutline({ mark }: { mark: UserAvatarMark }) {
  const common = {
    fill: 'none',
    stroke: 'currentColor',
    strokeLinecap: 'round' as const,
    strokeLinejoin: 'round' as const,
    strokeWidth: 1.7,
  };

  return (
    <svg className="user-avatar-mark" width="48%" height="48%" viewBox="0 0 24 24" aria-hidden="true" focusable="false">
      <g {...common}>
        <AvatarMarkPaths mark={mark} />
      </g>
    </svg>
  );
}

// 名册行（row）、详情身份栏（plate）、订阅与节点弹窗标题（mini），以及实时流量卡与流量预测
// 「用户占用」的列表格（cell）共用同一张身份牌。状态灯独立叠加在混合线稿右下角。
export function GeneratedUserAvatar({
  id,
  variant = 'row',
  lamp = null,
  self = false,
}: {
  id: string;
  variant?: 'row' | 'plate' | 'mini' | 'cell';
  lamp?: AvatarLamp | null;
  /** 调用方按租户与用户一起匹配，避免跨租户的同名账号被标为本人。 */
  self?: boolean;
}) {
  const shape =
    variant === 'plate'
      ? 'plate'
      : variant === 'mini'
        ? 'user-avatar mini'
        : variant === 'cell'
          ? 'user-avatar cell'
          : 'user-avatar';
  const identity = userAvatarIdentity(id);
  const style: UserAvatarStyle = { '--avatar-hue': identity.hue };

  return (
    <span
      className={`${shape} user-generated-avatar${self ? ' is-self' : ''}`}
      style={style}
      data-avatar-mark={identity.mark}
      aria-hidden={variant === 'cell' ? true : undefined}
    >
      <AvatarOutline mark={identity.mark} />
      {lamp && (
        <i className={`node-lamp${lamp.tone ? ` ${lamp.tone}` : ''}`} title={lamp.title} aria-label={lamp.title} />
      )}
    </span>
  );
}

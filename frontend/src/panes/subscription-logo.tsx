import clashLogo from '../ui/brands/clash-verge-rev.png';
import koipyLogo from '../ui/brands/koipy.png';
import subStoreLogo from '../ui/brands/substore-logo.svg';

const logos = { clash: clashLogo, haitun: koipyLogo, substore: subStoreLogo };

export function SubscriptionLogo({ card, small = false }: { card: keyof typeof logos; small?: boolean }) {
  return (
    <span className={`sub-pass-logo brand${small ? ' small' : ''}`} aria-hidden="true">
      <img src={logos[card]} alt="" />
    </span>
  );
}

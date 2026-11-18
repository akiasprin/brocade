// 组合台与实例详情共用的走线几何：量出 `data-anchor` 插座的位置，再按三次贝塞尔曲线连接。
// 插座落在卡片边缘，连线画在卡片下层；标签与拖动中的线另画一层，浮在卡片之上。
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react';

export type Point = { x: number; y: number };
export type Geometry = { anchors: Record<string, Point>; width: number; height: number };

/** 返回挂在走线容器上的 ref，以及容器内各 `data-anchor` 插座相对容器的中心点。 */
export function useAnchorGeometry<T extends HTMLElement>() {
  const root = useRef<T>(null);
  const [geo, setGeo] = useState<Geometry>({ anchors: {}, width: 0, height: 0 });
  const measure = useCallback(() => {
    const element = root.current;
    if (!element) return;
    const box = element.getBoundingClientRect();
    const anchors: Record<string, Point> = {};
    element.querySelectorAll<HTMLElement>('[data-anchor]').forEach(anchor => {
      const rect = anchor.getBoundingClientRect();
      // 隐藏的插座（窄屏或折叠的内容）没有尺寸，不能当成左上角的端点。
      if (!rect.width && !rect.height) return;
      anchors[anchor.dataset.anchor!] = {
        x: rect.left + rect.width / 2 - box.left,
        y: rect.top + rect.height / 2 - box.top,
      };
    });
    setGeo(old => {
      const next = { anchors, width: box.width, height: box.height };
      return JSON.stringify(old) === JSON.stringify(next) ? old : next;
    });
  }, []);
  useLayoutEffect(measure);
  useEffect(() => {
    const element = root.current;
    if (!element || typeof ResizeObserver === 'undefined') return;
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [measure]);
  return { root, geo };
}

const bend = (a: Point, b: Point) => Math.max(36, Math.abs(b.x - a.x) * 0.5);

export function curve(a: Point, b: Point) {
  const dx = bend(a, b);
  return `M${a.x} ${a.y} C${a.x + dx} ${a.y}, ${b.x - dx} ${b.y}, ${b.x} ${b.y}`;
}

export function along(a: Point, b: Point, t: number): Point {
  const dx = bend(a, b);
  const p1 = { x: a.x + dx, y: a.y };
  const p2 = { x: b.x - dx, y: b.y };
  const u = 1 - t;
  return {
    x: u * u * u * a.x + 3 * u * u * t * p1.x + 3 * u * t * t * p2.x + t * t * t * b.x,
    y: u * u * u * a.y + 3 * u * u * t * p1.y + 3 * u * t * t * p2.y + t * t * t * b.y,
  };
}

/** 线上的等宽标签：底色块按字符数估宽，避免压在端口或卡片边缘上时看不清。 */
export function WireLabel({ at, text }: { at: Point; text: string }) {
  const width = text.length * 6.1 + 12;
  return (
    <g className="cz-wire-label">
      <rect x={at.x - width / 2} y={at.y - 9} width={width} height={18} rx={6} />
      <text x={at.x} y={at.y + 3.5} textAnchor="middle">
        {text}
      </text>
    </g>
  );
}

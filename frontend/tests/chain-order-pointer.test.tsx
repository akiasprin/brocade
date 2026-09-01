import { describe, expect, it } from 'vitest';
import { orderForPointer, type OrderSlot } from '../src/panes/chains';

const slots: OrderSlot[] = [
  { id: 'a', left: 0, top: 0, width: 100, height: 80 },
  { id: 'b', left: 112, top: 0, width: 100, height: 80 },
  { id: 'c', left: 224, top: 0, width: 100, height: 80 },
  { id: 'd', left: 0, top: 92, width: 100, height: 80 },
];

describe('线路拖拽槽位判定', () => {
  it('指针停在同一个固定槽位时不会在相邻卡片之间反复交换', () => {
    const baseline = ['a', 'b', 'c', 'd'];
    const first = orderForPointer(baseline, 'a', slots, 162, 40);
    const next = orderForPointer(baseline, 'a', slots, 162, 40);

    expect(first).toEqual(['b', 'a', 'c', 'd']);
    expect(next).toEqual(first);
  });

  it('回到起始槽位会恢复起拖时的顺序', () => {
    expect(orderForPointer(['a', 'b', 'c', 'd'], 'a', slots, 50, 40)).toEqual(['a', 'b', 'c', 'd']);
  });

  it('四列换行后仍按二维槽位定位，并补偿自动滚动造成的视口位移', () => {
    expect(orderForPointer(['a', 'b', 'c', 'd'], 'b', slots, 50, 132)).toEqual(['a', 'c', 'd', 'b']);
    expect(orderForPointer(['a', 'b', 'c', 'd'], 'b', slots, 50, 112, 20)).toEqual(['a', 'c', 'd', 'b']);
  });
});

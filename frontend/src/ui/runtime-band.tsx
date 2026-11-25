import type { ReactNode } from 'react';
import { Icon, type IconName } from './icons';

/* 运行状态卡的两种结构件：横带（一个数据来源）与字段网格。机器详情与容器详情共用同一套，
   字段排版、断点和跨栏规则统一在 styles.css 的「运行状态卡」一节维护。 */

/** 一个字段格。第三个元素是跨栏数——网格是 8 栏，长值（CPU 型号、发行版、内核）
    占一栏放不下。跨栏数在窄容器下由 styles.css 按断点收窄，不在此处判断容器宽度。
    使用字面量而非数字，是为了在调用处能直接看出这是跨栏而不是别的计数。 */
export type RuntimeCellSpan = 'w2' | 'w3' | 'w5';
export type RuntimeCellItem = [string, ReactNode] | [string, ReactNode, RuntimeCellSpan];

export function RuntimeCells({ items }: { items: RuntimeCellItem[] }) {
  return (
    <dl className="nd-rt-cells">
      {items.map(([k, v, span]) => (
        <div className={span ? `nd-rt-c ${span}` : 'nd-rt-c'} key={k}>
          <dt>{k}</dt>
          <dd>{v}</dd>
        </div>
      ))}
    </dl>
  );
}

/** 一条横带：一个数据来源。各带的来源不同，因此每条带自带时效读数，
    合并成一张卡后不能只在卡头写一个时间。 */
export function RuntimeBand({
  name,
  icon,
  meta,
  children,
}: {
  name: string;
  icon: IconName;
  meta: ReactNode;
  children: ReactNode;
}) {
  return (
    <section className="nd-rt-band">
      <div className="nd-rt-bl">
        <b>
          <Icon of={icon} size={12} className="nd-rt-bl-ic" />
          {name}
        </b>
        <i>{meta}</i>
      </div>
      {children}
    </section>
  );
}

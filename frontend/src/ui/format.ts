const TiB = 1024 ** 4;
const GiB = 1024 ** 3;
const MiB = 1024 ** 2;

// 流量数值按 1024 进制转换为 KiB/MiB/GiB/TiB。小数位数随数量级递减：
// 数值越大，精确到 0.01 以下的必要性越低。
// TiB 一档用于月度合计——一个租户单月达到数 TiB 属于常见情况，
// 显示为「3564.82 GiB」需要计算位数才能判断量级。
export const bytes = (n: number) =>
  n >= TiB
    ? `${(n / TiB).toFixed(2)} TiB`
    : n >= GiB
      ? `${(n / GiB).toFixed(2)} GiB`
      : n >= MiB
        ? `${(n / MiB).toFixed(1)} MiB`
        : `${(n / 1024).toFixed(0)} KiB`;

export type HopWireKind = 'none' | 'encryption' | 'reality' | 'shadowsocks2022';

/** 中转端口的四档协议，数组顺序即下拉框中的顺序。
 *
 * 定义一份而非每个下拉框各定义一份：这四档原本分散在三处，添加 SS2022 时遗漏了检视页，
 * 导致 REALITY 显示为「无」——伪装程度最高的一档被显示为未加密。
 *
 * `reverseOk` 定义为数据而非各处的条件判断：反向隧道基于 VLESS 账号建立，
 * shadowsocks 没有对应的账号机制，因此该档在反向端口上不可选。编译器也会拒绝
 * （hop.reverse-needs-vless），但那是最后一道校验；能在下拉框中说明的不应等到发布时报错。
 */
export const HOP_WIRE_OPTIONS: {
  kind: HopWireKind;
  short: string;
  label: string;
  reverseOk: boolean;
}[] = [
  { kind: 'none', short: 'VLESS-NONE', label: 'VLESS-NONE（不加密）', reverseOk: true },
  { kind: 'encryption', short: 'VLESS-ENCRY', label: 'VLESS-ENCRY（加密）', reverseOk: true },
  { kind: 'reality', short: 'REALITY', label: 'REALITY（加密 + 伪装成真站点）', reverseOk: true },
  {
    kind: 'shadowsocks2022',
    short: 'SS2022',
    label: 'SS2022（加密，TCP/UDP 分流）',
    reverseOk: false,
  },
];

/** 简称，用于单行摘要和检视页。无法识别时按第一档处理，不留空。 */
export const hopWireLabel = (t: string | null | undefined): string =>
  HOP_WIRE_OPTIONS.find(option => option.kind === t)?.short ?? HOP_WIRE_OPTIONS[0].short;

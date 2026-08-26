// 界面上节点的标识使用 name，id 只用于技术引用：编辑控件的值、title 提示，
// 以及没有 name 时的回退。机器列表使用共享的 ['nodes'] 查询——需要名称的面板
// 通常已拉取过该数据，此处命中缓存，不产生额外请求。
import { useQuery } from '@tanstack/react-query';
import { fetchNodes } from '../api';

export function useNodeNames(): (id: string) => string {
  const q = useQuery({ queryKey: ['nodes'], queryFn: () => fetchNodes() });
  const map = q.data ? new Map(q.data.nodes.map(n => [n.node_id, n.name])) : null;
  return id => map?.get(id) || id;
}

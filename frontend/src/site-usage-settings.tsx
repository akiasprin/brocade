import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query';
import { fetchSiteUsagePolicy, saveSiteUsagePolicy } from './api';
import { ErrorBox, SegmentedControl } from './ui/bits';
import { PanelTitle } from './ui/icons';
import { OtherDomainSettingsSection, SniffingFailedSettingsSection } from './other-domain-settings';

export function SiteUsageSettingsCard({ editable }: { editable: boolean }) {
  return (
    <section className="panel config-panel" id="set-site-usage" aria-label="全局出流量统计">
      <header>
        <PanelTitle of="usage">出流量统计</PanelTitle>
      </header>
      <p className="cardsub">
        按用户和入口节点统计目的端的上下行流量，统计维度包括站点分类、未分类域名和嗅探失败目标，新安装默认全部开启，各项独立配置。
      </p>
      <div className="outflow-statistics-sections">
        <SiteClassificationSettings editable={editable} />
        <OtherDomainSettingsSection editable={editable} />
        <SniffingFailedSettingsSection editable={editable} />
      </div>
      <p className="settings-block-note">策略随节点轮询生效，无需发布或重启；统计结果仅管理员可见，不影响计费用量。</p>
      <p className="settings-block-note">
        域名与目标明细默认保留 24 小时，可在“数据保留”中分别调整。不记录 URL
        或请求内容。容量满时优先淘汰已可靠保存的冷条目；缓冲满时标记受限。调整容量会重建采集窗口。
      </p>
      {!editable && <p className="settings-block-note">仅系统管理员可修改。</p>}
    </section>
  );
}

function SiteClassificationSettings({ editable }: { editable: boolean }) {
  const client = useQueryClient();
  const policy = useQuery({ queryKey: ['site-usage-policy'], queryFn: fetchSiteUsagePolicy });
  const save = useMutation({
    mutationFn: saveSiteUsagePolicy,
    onSuccess: async value => {
      await client.cancelQueries({ queryKey: ['site-usage-policy'] });
      client.setQueryData(['site-usage-policy'], value);
      await client.invalidateQueries({ queryKey: ['user-site-usage'] });
    },
  });
  return (
    <section className="setgrp settings-block" aria-label="站点分类">
      <h5 className="eyebrow">站点分类</h5>
      <div className="setfld">
        <div className="v">
          <SegmentedControl
            ariaLabel="站点分类统计"
            value={policy.data?.enabled ?? false}
            options={[
              { value: false, label: '关闭' },
              { value: true, label: '开启' },
            ]}
            disabled={!editable || !policy.data || policy.isError || save.isPending}
            onChange={enabled => save.mutate(enabled)}
          />
        </div>
      </div>
      <p className="settings-block-note">按内置站点分类汇总流量；开启后对新连接生效。</p>
      {policy.error && <ErrorBox error={policy.error} />}
      {save.error && <ErrorBox error={save.error} />}
      {policy.isPending && (
        <p className="settings-block-note" role="status">
          读取中…
        </p>
      )}
      {save.isPending && (
        <p className="settings-block-note" role="status">
          正在保存…
        </p>
      )}
      {save.isSuccess && (
        <p className="settings-block-note" role="status">
          已保存，待节点同步。
        </p>
      )}
    </section>
  );
}

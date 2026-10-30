import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { NodeAgentStateItem, NodeUnlockProbe } from '../src/api';
import { NodeUnlockPanel } from '../src/panes/node-unlock';
import { youtubeSplitResult } from './fixtures/youtube-unlock-data';
const machine = { node_id: 'one/edge', agent_protocol_version: 50, lifecycle_phase: 'active' } as NodeAgentStateItem;
const completed: NodeUnlockProbe = { id: 'probe', family: 'v4', node_id: machine.node_id, state: 'complete', created_at: 1, completed_at: 2, results: [
  { service: 'chatgpt', status: 'unsupported_region', code: 'region_unsupported', region: 'HK' },
  { service: 'netflix', status: 'available', code: 'web_available', region: 'JP' },
  { service: 'tiktok', status: 'failed', code: 'timeout', region: null },
  { service: 'reddit', status: 'restricted', code: 'access_denied', region: null },
] };
function show(node = machine, editable = true) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false }, mutations: { retry: false } } });
  return render(<QueryClientProvider client={client}><NodeUnlockPanel node={node} editable={editable} /></QueryClientProvider>);
}
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });
describe('机器解锁磁贴', () => {
  it('只在点击后发起，使用机器作用域并接续轮询', async () => {
    const fetcher = vi.fn(async (url, init) => { if (url.endsWith('/capabilities')) return Response.json({ipv6_available:true,ipv6_reason:null}); expect(url).toBe('/nodes/one%2Fedge/unlock-probe?family=v4'); return Response.json(init?.method === 'POST' ? { ...completed, state: 'queued', results: [], created_at: Date.now()/1000 } : null); });
    vi.stubGlobal('fetch', fetcher); const view = show();
    const start = await view.findByRole('button', { name: '开始检测' });
    expect(fetcher.mock.calls.every(([, init]) => !init?.method || init.method === 'GET')).toBe(true);
    fireEvent.click(start); await view.findByText('等待机器响应');
    expect(view.getByRole('button', { name: '检测中…' }).hasAttribute('disabled')).toBe(true);
    expect(view.getAllByText('等待结果')).toHaveLength(13);
    expect(fetcher.mock.calls.filter(([, init]) => init?.method === 'POST')).toHaveLength(1);
  });
  it('十三项位置固定，各自地区与结论独立，检测失败不计入不可用', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => Response.json(completed))); const view=show();
    await view.findByLabelText('可用 1 / 13'); expect(view.getAllByRole('article')).toHaveLength(13);
    expect(within(view.getByRole('article',{name:'ChatGPT'})).getByText('HK')).toBeTruthy();
    expect(within(view.getByRole('article',{name:'ChatGPT'})).getByText('地区不支持')).toBeTruthy();
    expect(within(view.getByRole('article',{name:'Netflix'})).getByText('JP')).toBeTruthy();
    const failed=view.getByRole('article',{name:'TikTok'}); expect(failed.dataset.tone).toBe('unknown');
    expect(within(failed).getByText('检测失败 · 请求超时')).toBeTruthy(); expect(within(failed).getByText('—')).toBeTruthy();
  });
  it.each([
    ['HKG', 'HK'], ['SIN', 'SG'], ['BKK', 'TH'], ['IAD', 'US'], ['LHR', 'GB'], ['HK', 'HK'], ['XXX', null],
  ] as const)('Reddit 保留 %s 原码并为已知 POP 显示 %s 旗帜', async (region, flag) => {
    vi.stubGlobal('fetch', vi.fn(async () => Response.json({ ...completed, results: [
      { service: 'reddit', status: 'available', code: 'web_available', region },
    ] })));
    const view = show(); const tile = await view.findByRole('article', { name: 'Reddit' });
    expect(within(tile).getAllByText(region)).toHaveLength(1);
    expect(within(tile).getByText('可用')).toBeTruthy();
    if (flag) expect(within(tile).getByRole('img', { name: `${flag} 地区旗` })).toBeTruthy();
    else expect(within(tile).queryByRole('img')).toBeNull();
    expect(tile.textContent).not.toContain('CDN');
  });
  it('扩展服务显示地区和结论，已移除的三项不显示也不计数', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => Response.json({ ...completed, results: [
      { service:'spotify',status:'available',code:'web_available',region:'HK' },
      { service:'hbo_max',status:'available',code:'web_available',region:'HK' },
      { service:'dazn',status:'unsupported_region',code:'region_unsupported',region:'CN' },
      { service:'bbc_iplayer',status:'available',code:'web_available',region:'GB' },
      { service:'crunchyroll',status:'available',code:'web_available',region:'JP' },
      { service:'paramount_plus',status:'skipped',code:'no_ipv6_dns',region:null },
    ] })));
    const view=show();
    const spotify=await view.findByRole('article',{name:'Spotify'});
    expect(within(spotify).getByText('可用')).toBeTruthy();expect(within(spotify).getByText('HK')).toBeTruthy();expect(spotify.textContent).not.toContain('注册');
    expect(within(view.getByRole('article',{name:'HBO Max'})).getByText('HK')).toBeTruthy();
    expect(within(view.getByRole('article',{name:'DAZN'})).getByText('地区不支持')).toBeTruthy();
    expect(view.queryByRole('article',{name:'BBC iPlayer'})).toBeNull();
    expect(view.queryByRole('article',{name:'Crunchyroll'})).toBeNull();
    expect(view.queryByRole('article',{name:'Paramount+'})).toBeNull();
    await view.findByLabelText('可用 2 / 13');
  });
  it('Gemini 的三位国家码使用 ISO 映射旗帜并保留原码', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => Response.json({ ...completed, results: [
      { service: 'gemini', status: 'available', code: 'web_available', region: 'THA' },
    ] })));
    const view = show(); const tile = await view.findByRole('article', { name: 'Gemini' });
    expect(within(tile).getAllByText('THA')).toHaveLength(1);
    expect(within(tile).getByRole('img', { name: 'TH 地区旗' })).toBeTruthy();
  });
  it('Spotify 使用通用访问状态，不显示旧文案', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => Response.json({ ...completed, results: [
      { service: 'spotify', status: 'restricted', code: 'access_denied', region: 'HK' },
    ] })));
    const view = show(); const tile = await view.findByRole('article', { name: 'Spotify' });
    expect(within(tile).getByText('访问受限')).toBeTruthy();
    expect(within(tile).getByText('HK')).toBeTruthy();
    expect(tile.textContent).not.toContain('注册');
    expect(tile.title).not.toContain('注册');
  });
  it('历史客户端 DNS 明确展示条件与服务返回地区', async () => {
    vi.stubGlobal('fetch',vi.fn(async()=>Response.json({...completed,results:[{service:'youtube_premium',status:'available',code:'web_available',region:'HK',client_dns:true}]})));
    const view=show(); const tile=await view.findByRole('article',{name:'YouTube Premium'});
    expect(within(tile).getByText('客户端 DNS')).toBeTruthy(); expect(tile.title).toContain('同一链路');
    expect(within(tile).getByText('HK')).toBeTruthy();
  });
  it('YouTube 要点与弹层分别呈现网页、Premium、GL、隐私地区与 CDN',async()=>{
    vi.stubGlobal('fetch',vi.fn(async()=>Response.json({...completed,results:[youtubeSplitResult]})));const view=show();
    const tile=await view.findByRole('article',{name:'YouTube'});expect(tile.dataset.tone).toBe('ok');expect(within(tile).queryByText('可用')).toBeNull();expect(tile.textContent).not.toContain('地区不支持');expect(tile.textContent).toContain('视频 CDN HKG');expect(tile.textContent).not.toContain('香港');const premium=view.getByRole('article',{name:'YouTube Premium'});expect(within(premium).getByText('提示 · 地区不支持')).toBeTruthy();expect(premium.dataset.tone).toBe('info');await view.findByLabelText('可用 1 / 13');
    expect(view.queryByRole('region',{name:'YouTube 检测详情'})).toBeNull();fireEvent.click(within(tile).getByRole('button',{name:'YouTube 分项'}));
    const detail=await view.findByRole('region',{name:'YouTube 检测详情'});
    expect(within(within(detail).getByText('地区').parentElement!).getByText('MO')).toBeTruthy();
    expect(within(within(detail).getByText('页面配置地区').parentElement!).getByText('US')).toBeTruthy();
    expect(within(within(detail).getByText('隐私元数据 · 地区').parentElement!).getByText('MO')).toBeTruthy();expect(detail.textContent).toContain('hkg12s15');
    expect(within(within(detail).getByText('YouTube 网页').parentElement!).getByText('可用')).toBeTruthy();
    expect(within(within(detail).getByText('YouTube Premium').parentElement!).getByText('提示 · 地区不支持')).toBeTruthy();
    fireEvent.click(view.getByRole('button',{name:'关闭 YouTube 分项'}));await waitFor(()=>expect(view.queryByRole('region',{name:'YouTube 检测详情'})).toBeNull());
  });
  it('CDN 失败和未知地区不覆盖网页事实',async()=>{
    const result=structuredClone(youtubeSplitResult);result.region=null;result.youtube!.premium.privacy_region=null;result.youtube!.webpage.privacy_region=null;result.youtube!.cdn={http_status:null,node:null,airport:null,country:null,error:'timeout',elapsed_ms:0};
    vi.stubGlobal('fetch',vi.fn(async()=>Response.json({...completed,results:[result]})));const view=show();const tile=await view.findByRole('article',{name:'YouTube'});
    expect(tile.dataset.tone).toBe('ok');expect(within(tile).queryByText('可用')).toBeNull();expect(tile.textContent).toContain('视频 CDN 请求超时');expect(within(tile).getByText('—')).toBeTruthy();
  });
  it('YouTube 与 Premium 分别使用自己的状态和地区，旧结果不能推断网页可用',async()=>{
    const result=structuredClone(youtubeSplitResult);result.youtube!.premium.privacy_region='SG';
    vi.stubGlobal('fetch',vi.fn(async()=>Response.json({...completed,results:[result]})));const view=show();
    const web=await view.findByRole('article',{name:'YouTube'});const premium=view.getByRole('article',{name:'YouTube Premium'});
    expect(within(web).getByText('MO')).toBeTruthy();expect(web.dataset.tone).toBe('ok');expect(within(web).queryByText('可用')).toBeNull();
    expect(within(premium).getByText('SG')).toBeTruthy();expect(within(premium).getByText('提示 · 地区不支持')).toBeTruthy();view.unmount();
    vi.stubGlobal('fetch',vi.fn(async()=>Response.json({...completed,results:[{service:'youtube_premium',status:'available',code:'web_available',region:'HK'}]})));
    const legacy=show();const unknown=await legacy.findByRole('article',{name:'YouTube'});
    expect(within(unknown).getByText('未取得')).toBeTruthy();expect(within(unknown).getByText('—')).toBeTruthy();
    expect(within(legacy.getByRole('article',{name:'YouTube Premium'})).getByText('可用')).toBeTruthy();
  });
  it('Premium 的变化只标在 Premium 项，网页不继承它的历史结论',async()=>{
    const previous=structuredClone(youtubeSplitResult);previous.status='available';previous.code='web_available';previous.region='SG';
    previous.youtube!.premium={...previous.youtube!.premium,status:'available',code:'web_available',privacy_region:'SG'};
    vi.stubGlobal('fetch',vi.fn(async()=>Response.json({...completed,results:[youtubeSplitResult],previous_results:[previous]})));const view=show();
    const web=await view.findByRole('article',{name:'YouTube'});expect(web.textContent).not.toContain('上次');
    expect(view.getByRole('article',{name:'YouTube Premium'}).textContent).toContain('上次 可用 · SG');
  });
  it('旧 Agent 禁止开始，只读角色没有操作',async()=>{
    vi.stubGlobal('fetch',vi.fn(async()=>Response.json(null)));const old=show({...machine,agent_protocol_version:45});await old.findByText('需要升级 Agent');expect(old.queryByRole('button',{name:'开始检测'})).toBeNull();old.unmount();
    const view=show(machine,false);await view.findByText(/联系管理员/);expect(view.queryByRole('button',{name:'开始检测'})).toBeNull();
  });
  it('读取和发起失败提供重试，不冒充空结果',async()=>{
    const fetcher=vi.fn(async()=>Response.json({error:'读取失败'},{status:503}));vi.stubGlobal('fetch',fetcher);const view=show();const retry=await view.findByRole('button',{name:'重试读取'});
    expect(view.queryByText('尚未检测')).toBeNull();fetcher.mockImplementation(async()=>Response.json(null));fireEvent.click(retry);const start=await view.findByRole('button',{name:'开始检测'});
    fetcher.mockImplementation(async()=>Response.json({error:'机器需要升级'},{status:409}));fireEvent.click(start);await view.findByText(/机器需要升级/);
  });
  for(const state of ['running','expired','path'] as const) it(`${state} 保留上次结果并减淡`,async()=>{
    vi.stubGlobal('fetch',vi.fn(async()=>Response.json({...completed,state:state==='path'?'complete':state,results:state==='path'?completed.results.map(r=>({...r,status:'failed',code:'network_error',region:null})):[],previous_results:completed.results,previous_completed_at:2,path_error:state==='path'?'client_unavailable':null})));
    const view=show();const tile=await view.findByRole('article',{name:'Netflix'});expect(within(tile).getByText('JP')).toBeTruthy();expect(tile.closest('[data-stale]')).toBeTruthy();expect(view.getByText(/的结果/)).toBeTruthy();
  });
  it('完成后真实对比上次读数，时间保留精确时刻',async()=>{
    vi.stubGlobal('fetch',vi.fn(async()=>Response.json({...completed,previous_results:[{service:'netflix',status:'partial',code:'originals_only',region:'SG'}],next_run_at:Date.now()/1000+86400})));const view=show();
    const tile=await view.findByRole('article',{name:'Netflix'});expect(tile.textContent).toContain('上次 仅自制剧 · SG');expect(view.container.querySelector('time[datetime][title]')).toBeTruthy();
  });
});

it('上次检测失败不作为历史读数显示，当前失败仍保留原因',async()=>{
 vi.stubGlobal('fetch',vi.fn(async()=>Response.json({...completed,previous_results:[{service:'netflix',status:'failed',code:'network_error',region:null}]})));
 const view=show();const tile=await view.findByRole('article',{name:'Netflix'});expect(within(tile).getByText('JP')).toBeTruthy();expect(tile.textContent).not.toContain('上次');
 expect(view.getByRole('article',{name:'TikTok'}).textContent).toContain('检测失败 · 请求超时');
});

it('IPv4 与 IPv6 使用独立结果、冷却与重测请求', async () => {
 const fetcher=vi.fn(async(url: string,init?:RequestInit)=>{
  if (url.endsWith('/capabilities')) return Response.json({ipv6_available:true,ipv6_reason:null});
  const family=new URL(url,'http://localhost').searchParams.get('family');
  expect(['v4','v6']).toContain(family);
  const item={...completed,family,created_at:family==='v4'?Date.now()/1000:1,results:[{service:'netflix',status:'available',code:'web_available',region:family==='v4'?'JP':'US'}]};
  return Response.json(init?.method==='POST'?{...item,state:'queued',results:[],previous_results:item.results,previous_completed_at:2}:item);
 });
 vi.stubGlobal('fetch',fetcher);const view=show();
 const tile=await view.findByRole('article',{name:'Netflix'});expect(within(tile).getByText('JP')).toBeTruthy();
 expect(view.getByRole('button',{name:/秒后可重测/}).hasAttribute('disabled')).toBe(true);
 fireEvent.click(view.getByRole('button',{name:'IPv6'}));
 await waitFor(()=>expect(view.getByRole('article',{name:'Netflix'}).textContent).toContain('US'));
 fireEvent.click(view.getByRole('button',{name:'重新检测'}));await view.findByText('等待机器响应');
 expect(fetcher.mock.calls.filter(([,init])=>init?.method==='POST').map(([url])=>url)).toEqual(['/nodes/one%2Fedge/unlock-probe?family=v6']);
 fireEvent.click(view.getByRole('button',{name:'IPv4'}));
 await waitFor(()=>expect(view.getByRole('article',{name:'Netflix'}).textContent).toContain('JP'));
 expect(view.queryByText('等待机器响应')).toBeNull();
});

it('切换地址族时，延迟返回的 IPv4 发起响应不能覆盖 IPv6',async()=>{
 let release:((response:Response)=>void)|undefined;
 vi.stubGlobal('fetch',vi.fn(async(url:string,init?:RequestInit)=>{
  if(init?.method==='POST')return await new Promise<Response>(resolve=>{release=resolve;});
  const family=new URL(url,'http://localhost').searchParams.get('family');
  return Response.json({...completed,family,results:[{service:'netflix',status:'available',code:'web_available',region:family==='v4'?'JP':'US'}]});
 }));
 const view=show();fireEvent.click(await view.findByRole('button',{name:'重新检测'}));await waitFor(()=>expect(release).toBeTruthy());
 fireEvent.click(view.getByRole('button',{name:'IPv6'}));
 await waitFor(()=>expect(view.getByRole('article',{name:'Netflix'}).textContent).toContain('US'));
 expect(view.getByRole('button',{name:'重新检测'}).hasAttribute('disabled')).toBe(false);
 release!(Response.json({...completed,state:'queued',results:[],created_at:Date.now()/1000}));
 await waitFor(()=>expect(view.getByRole('button',{name:'重新检测'}).hasAttribute('disabled')).toBe(false));
 expect(view.getByRole('article',{name:'Netflix'}).textContent).toContain('US');
});

it('自动选路的旧记录不归入任一族',async()=>{
 vi.stubGlobal('fetch',vi.fn(async()=>Response.json({...completed,family:null})));const view=show();
 await view.findByText('旧结果未区分地址族');expect(view.queryAllByRole('article')).toHaveLength(0);
 fireEvent.click(view.getByRole('button',{name:'IPv6'}));await view.findByText('旧结果未区分地址族');expect(view.queryAllByRole('article')).toHaveLength(0);
});

it('不同地址族的错误响应显式报错，不显示成当前族结果',async()=>{
 vi.stubGlobal('fetch',vi.fn(async()=>Response.json({...completed,family:'v6'})));const view=show();
 await view.findByText(/地址族不一致/);expect(view.queryAllByRole('article')).toHaveLength(0);
});

it('本机地址观测不禁用 IPv6 实测，实际无 AAAA 才显示跳过', async () => {
 const fetcher=vi.fn(async(url:string)=>Response.json(url.endsWith('/capabilities')
  ? {ipv6_available:false,ipv6_reason:'机器无 IPv6 地址'}
  : {...completed,family:url.includes('family=v6')?'v6':'v4',results:[{service:'reddit',status:'skipped',code:'no_ipv6_dns',region:null}]}));
 vi.stubGlobal('fetch',fetcher);const view=show();
 const ipv6=await view.findByRole('button',{name:'IPv6'});
 expect(ipv6.hasAttribute('disabled')).toBe(false);
 fireEvent.click(ipv6);await waitFor(()=>expect(fetcher.mock.calls.some(([url])=>url.includes('family=v6'))).toBe(true));
 const tile=await view.findByRole('article',{name:'Reddit'});
 expect(tile.textContent).toContain('已跳过 · 无 IPv6 地址解析');
 expect(tile.textContent).not.toContain('检测失败');
});

it('纯字母 CDN 映射显示原始节点码，位置未知不冒充服务验证失败',async()=>{
 const result=structuredClone(youtubeSplitResult);
 result.youtube!.cdn={http_status:200,node:'nctsaa-ae',airport:null,country:null,error:null,elapsed_ms:58};
 vi.stubGlobal('fetch',vi.fn(async()=>Response.json({...completed,results:[result]})));
 const view=show();const tile=await view.findByRole('article',{name:'YouTube'});
 expect(tile.textContent).toContain('视频 CDN nctsaa-ae');
 expect(tile.textContent).not.toContain('未识别服务响应');
 expect(tile.textContent).not.toContain('TSA');
});

it('视频 CDN 无法识别映射时只说明映射未知，不推断需要验证',async()=>{
 const result=structuredClone(youtubeSplitResult);
 result.youtube!.cdn={http_status:200,node:null,airport:null,country:null,error:'unrecognized_response',elapsed_ms:58};
 vi.stubGlobal('fetch',vi.fn(async()=>Response.json({...completed,results:[result]})));
 const view=show();const tile=await view.findByRole('article',{name:'YouTube'});
 expect(tile.textContent).toContain('视频 CDN 映射响应未识别');
 expect(tile.textContent).not.toContain('可能需要验证');
 expect(tile.dataset.tone).toBe('ok');
 fireEvent.click(within(tile).getByRole('button',{name:'YouTube 分项'}));
 const detail=await view.findByRole('region',{name:'YouTube 检测详情'});
 expect(detail.textContent).toContain('映射响应未识别');
 expect(detail.textContent).not.toContain('可能需要验证');
});

it('Premium 的提示仅适用于订阅地区限制，网页失败与其他服务限制仍保留', async () => {
 const result=structuredClone(youtubeSplitResult);
 result.youtube!.webpage.status='failed';result.youtube!.webpage.code='network_error';
 vi.stubGlobal('fetch',vi.fn(async()=>Response.json({...completed,results:[result,...completed.results]})));
 const view=show();const premium=await view.findByRole('article',{name:'YouTube Premium'});
 expect(premium.dataset.tone).toBe('info');expect(premium.textContent).toContain('提示 · 地区不支持');
 const webpage=view.getByRole('article',{name:'YouTube'});expect(webpage.dataset.tone).toBe('unknown');expect(webpage.textContent).toContain('网络连接失败');
 expect(view.getByRole('article',{name:'ChatGPT'}).dataset.tone).toBe('no');
 await view.findByLabelText('可用 1 / 13');
});

it.each(['CN', 'MO'])('YouTube %s 地区限制统一为提示且不计为可用', async region => {
  const result = structuredClone(youtubeSplitResult);
  result.region = region;
  for (const part of ['webpage', 'premium'] as const) {
    result.youtube![part].status = 'unsupported_region';
    result.youtube![part].code = 'region_unsupported';
    result.youtube![part].privacy_region = region;
  }
  vi.stubGlobal('fetch', vi.fn(async () => Response.json({ ...completed, results: [result] })));
  const view = show();
  for (const service of ['YouTube', 'YouTube Premium']) {
    const tile = await view.findByRole('article', { name: service });
    expect(tile.dataset.tone).toBe('info');
    expect(tile.textContent).toContain('提示 · 地区不支持');
    expect(within(tile).getByText(region)).toBeTruthy();
  }
  await view.findByLabelText('可用 0 / 13');
});

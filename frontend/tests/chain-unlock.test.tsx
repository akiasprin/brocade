import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, fireEvent, render, waitFor, within } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { ChainUnlockControl } from '../src/panes/node-unlock';
const job={id:'chain-test',family:'v4',node_id:'head',state:'complete',created_at:1,completed_at:2,protocol:'VLESS REALITY',results:[{service:'chatgpt',status:'available',code:'web_available',region:'TW'},{service:'netflix',status:'available',code:'web_available',region:'JP'}],next_run_at:Date.now()/1000+86400};
afterEach(()=>{cleanup();vi.unstubAllGlobals();});
function show(editable=true){return render(<QueryClientProvider client={new QueryClient({defaultOptions:{queries:{retry:false}}})}><ChainUnlockControl app="app-one" chain="chain-one" name="香港 → 东京" editable={editable}/></QueryClientProvider>);}
it('线路入口先读取数量，弹窗与按钮复用一个查询和手动检测',async()=>{
 const fetcher=vi.fn(async(url,init)=>{if (url.endsWith('/capabilities')) return Response.json({ipv6_available:true,ipv6_reason:null});expect(url).toBe('/apps/app-one/chains/chain-one/unlock-probe?family=v4');return Response.json(init?.method==='POST'?{...job,state:'queued',results:[],previous_results:job.results,previous_completed_at:2}:job);});vi.stubGlobal('fetch',fetcher);
 const view=show();await within(view.getByRole('button',{name:'解锁'})).findByText('2/13');expect(fetcher).toHaveBeenCalledTimes(2);fireEvent.click(view.getByRole('button',{name:'解锁'}));
 const dialog=await view.findByRole('dialog',{name:'区域性服务解锁状态 · 香港 → 东京'});expect(fetcher).toHaveBeenCalledTimes(2);expect(within(dialog).getByText('VLESS REALITY')).toBeTruthy();
 expect(within(within(dialog).getByRole('article',{name:'ChatGPT'})).getByText('TW')).toBeTruthy();expect(within(within(dialog).getByRole('article',{name:'Netflix'})).getByText('JP')).toBeTruthy();
 fireEvent.click(within(dialog).getByRole('button',{name:'重新检测'}));await within(dialog).findByText('等待机器响应');expect(fetcher.mock.calls.filter(([,init])=>init?.method==='POST')).toHaveLength(1);
 fireEvent.click(within(dialog).getByRole('button',{name:'关闭解锁'}));await waitFor(()=>expect(view.queryByRole('dialog')).toBeNull());
});
it('只读角色可看弹窗，无手动检测',async()=>{vi.stubGlobal('fetch',vi.fn(async()=>Response.json(job)));const view=show(false);fireEvent.click(view.getByRole('button',{name:'解锁'}));await view.findByRole('article',{name:'Netflix'});expect(view.queryByRole('button',{name:'重新检测'})).toBeNull();});
it('线路变更保持结果亮度，修订提示、下次时间和协议独立置底',async()=>{
 vi.stubGlobal('fetch',vi.fn(async()=>Response.json({...job,state:'obsolete',configuration_revision:2214})));const view=show();
 fireEvent.click(view.getByRole('button',{name:'解锁'}));const dialog=await view.findByRole('dialog',{name:'区域性服务解锁状态 · 香港 → 东京'});
 const tile=await within(dialog).findByRole('article',{name:'Netflix'});expect(within(tile).getByText('JP')).toBeTruthy();expect(tile.closest('[data-stale]')).toBeNull();
 expect(dialog.querySelector('.unlock-b-hero')?.hasAttribute('data-stale')).toBe(false);
 const footer=dialog.querySelector('.unlock-footer') as HTMLElement;expect(footer).toBeTruthy();expect(footer.textContent).toContain('线路已变更 · 结果来自修订 2214');expect(footer.textContent).toContain('下次');expect(footer.textContent).toContain('VLESS REALITY');
 expect(footer.querySelector('.unlock-meta-lead')?.getAttribute('data-tone')).toBe('warn');
 expect(dialog.querySelector('.unlock-bar')?.textContent).not.toContain('VLESS REALITY');expect(footer.parentElement?.classList.contains('chain-unlock-dialog')).toBe(true);
});

it('机器环境观测不禁用链路 IPv6 实测',async()=>{
 const fetcher=vi.fn(async(url:string)=>Response.json(url.endsWith('/capabilities')
  ?{family_mode:'split',ipv6_available:false,ipv6_reason:'链路出口无 IPv6 地址'}:{...job,family:url.includes('family=v6')?'v6':'v4'}));
 vi.stubGlobal('fetch',fetcher);const view=show();fireEvent.click(view.getByRole('button',{name:'解锁'}));
 const dialog=await view.findByRole('dialog');const ipv6=within(dialog).getByRole('button',{name:'IPv6'});
 expect(ipv6.hasAttribute('disabled')).toBe(false);fireEvent.click(ipv6);
 await waitFor(()=>expect(fetcher.mock.calls.some(([url])=>url.includes('family=v6'))).toBe(true));
 expect(within(dialog).queryByText('链路出口无 IPv6 地址')).toBeNull();
});

it('不支持强制分族的线路只显示一个不分族选项，使用独立出口任务',async()=>{
 const requests:string[]=[];
 vi.stubGlobal('fetch',vi.fn(async(url:string,init?:RequestInit)=>{
  requests.push(url);
  if(url.endsWith('/capabilities'))return Response.json({family_mode:'automatic',automatic_ready:true,ipv6_available:null,ipv6_reason:null});
  if(url.includes('automatic=true'))return Response.json({...job,automatic:true,family:null,state:init?.method==='POST'?'queued':'complete',results:init?.method==='POST'?[]:job.results});
  return Response.json({...job,path_error:'family_unavailable',results:[]});
 }));
 const view=show();fireEvent.click(view.getByRole('button',{name:'解锁'}));const dialog=await view.findByRole('dialog');
 const automatic=await within(dialog).findByRole('button',{name:'不分族'});
 expect(automatic.closest('.unlock-footer')).toBeTruthy();
 expect(within(dialog).queryByRole('button',{name:'IPv4'})).toBeNull();
 expect(within(dialog).queryByRole('button',{name:'IPv6'})).toBeNull();
 expect(await within(within(dialog).getByRole('article',{name:'Netflix'})).findByText('JP')).toBeTruthy();
 expect(dialog.textContent).not.toContain('旧结果未区分地址族');
 fireEvent.click(within(dialog).getByRole('button',{name:'重新检测'}));await within(dialog).findByText('等待机器响应');
 expect(requests.some(url=>url.endsWith('?automatic=true'))).toBe(true);
});

it('分族配置尚未就绪明确显示等待重试并保留旧结果',async()=>{
 vi.stubGlobal('fetch',vi.fn(async(url:string)=>Response.json(url.endsWith('/capabilities')
  ?{family_mode:'split',ipv6_available:null,ipv6_reason:null}:{...job,path_error:'family_unavailable',results:[],previous_results:job.results,previous_completed_at:2,next_run_at:Date.now()/1000+60})));
 const view=show();fireEvent.click(view.getByRole('button',{name:'解锁'}));const dialog=await view.findByRole('dialog');
 expect(await within(dialog).findByText('检测未就绪，稍后重试')).toBeTruthy();
 expect(within(dialog).queryByText('需要更新线路配置')).toBeNull();
});

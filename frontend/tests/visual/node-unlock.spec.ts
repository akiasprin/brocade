import { test, expect } from '@playwright/test';
import type { Locator } from '@playwright/test';
async function expectPremiumTitle(panel:Locator){
 const name=panel.getByRole('article',{name:'YouTube Premium',exact:true}).locator('.unlock-b-name');
 await expect(name).toHaveText('YouTube Premium');
 await name.evaluate(async()=>{await document.fonts.ready;});
 const geometry=await name.evaluate(el=>{const range=document.createRange();range.selectNodeContents(el);return {lines:range.getClientRects().length,textWidth:range.getBoundingClientRect().width,available:el.clientWidth};});
 expect(geometry.lines).toBe(1);expect(geometry.textWidth).toBeLessThanOrEqual(geometry.available+1);
}
const nodeScenes=['typical','clear','macau','running','queued','never','expired','old'];
const chainScenes=['typical','obsolete','path','running','queued','never','expired'];
for(const scope of ['node','chain']) for(const scene of scope==='node'?nodeScenes:chainScenes) for(const width of [1440,390]) {
  test(`${scope} ${scene} ${width}: B tiles`,async({page})=>{
    const errors:string[]=[];page.on('pageerror',e=>errors.push(e.message));
    page.on('response',r=>{if(r.status()>=400)errors.push(`${r.status()} ${r.url()}`);});
    await page.clock.setFixedTime(new Date('2026-10-26T08:00:00Z'));
    await page.setViewportSize({width,height:1100});
    await page.goto(`/tests/fixtures/node-unlock-preview.html?scope=${scope}&scene=${scene}`);
    const panel=scope==='chain'?page.getByRole('dialog',{name:/解锁/}):page.getByRole('tabpanel',{name:'解锁'});
    if(scope==='chain')await page.getByRole('button',{name:'解锁',exact:true}).click();
    await expect(panel.locator('.unlock-footer').getByRole('group',{name:'解锁地址族'})).toHaveCount(1);
    if(scene==='old')await expect(panel.getByText('需要升级 Agent',{exact:true})).toBeVisible();
    else if(scene==='never')await expect(panel.getByText('尚未检测',{exact:true})).toBeVisible();
    else {await expect(panel.getByRole('article')).toHaveCount(13);await expectPremiumTitle(panel);}
    if(scene==='path') {await expect(panel.getByText('入口无法启动检测客户端')).toBeVisible();await expect(panel.getByRole('article',{name:'Netflix'})).toContainText('JP');}
    if(['running','queued','expired','path'].includes(scene))await expect(panel.locator('.unlock-body')).toHaveAttribute('data-stale','');
    if(scene==='obsolete'){
      await expect(panel.locator('.unlock-body')).not.toHaveAttribute('data-stale','');
      expect(await panel.locator('.unlock-body').evaluate(el=>getComputedStyle(el).opacity)).toBe('1');
      await expect(panel.locator('.unlock-b-hero')).not.toHaveAttribute('data-stale','');
      const footer=panel.locator('.unlock-footer');await expect(footer).toContainText('线路已变更');await expect(footer).toContainText('结果来自修订');await expect(footer).toContainText('下次');await expect(footer).toContainText('VLESS REALITY');
      await expect(footer.locator('.unlock-meta-lead')).toHaveAttribute('data-tone','warn');
    }
    if(scene==='typical')await expect(panel.getByRole('article',{name:'ChatGPT'})).toContainText(scope==='chain'?'US':'HK');
    if(scene==='macau'){await expect(panel.getByRole('article',{name:'YouTube Premium'})).toContainText('MO');await expect(panel.getByRole('article',{name:'YouTube',exact:true})).toHaveAttribute('data-tone','ok');await expect(panel.getByRole('article',{name:'YouTube',exact:true})).not.toContainText('可用');await expect(panel.getByRole('article',{name:'YouTube',exact:true})).toContainText('视频 CDN HKG');await expect(panel.getByRole('article',{name:'YouTube',exact:true})).not.toContainText('地区不支持');await expect(panel.getByRole('article',{name:'YouTube Premium'})).toContainText('地区不支持');}
    await page.evaluate(()=>document.fonts.ready);
    expect(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth)).toBe(false);
    if(scope==='chain'){
      const surface=panel.locator('.chain-unlock-dialog');const box=await surface.boundingBox();expect(box!.x).toBeGreaterThanOrEqual(width===390?0:8);expect(box!.x+box!.width).toBeLessThanOrEqual(width===390?width:width-8);
      if(scene!=='never'){const body=await panel.locator('.unlock-dialog-body').boundingBox();const footer=await panel.locator('.unlock-footer').boundingBox();expect(footer!.y).toBeGreaterThanOrEqual(body!.y+body!.height-1);}
      if(width===390 && scene!=='never'){await panel.getByRole('article',{name:'Reddit'}).scrollIntoViewIfNeeded();await expect(panel.getByRole('button',{name:'关闭解锁'})).toBeVisible();await panel.locator('.unlock-dialog-body').evaluate(el=>el.scrollTop=0);}
    }
    await expect(page).toHaveScreenshot(`b-${scope}-${scene}-${width}.png`);
    expect(errors).toEqual([]);
  });
}
for(const scope of ['node','chain']) test(`${scope} light readonly`,async({page})=>{
  await page.clock.setFixedTime(new Date('2026-10-26T08:00:00Z'));await page.setViewportSize({width:1200,height:1100});
  await page.goto(`/tests/fixtures/node-unlock-preview.html?scope=${scope}&scene=typical&theme=light&readonly=1`);
  if(scope==='chain')await page.getByRole('button',{name:'解锁',exact:true}).click();
  await expect(page.getByRole('article')).toHaveCount(13);await expect(page.getByRole('button',{name:'重新检测'})).toHaveCount(0);
  await expectPremiumTitle(page.locator('body'));
  await expect(page).toHaveScreenshot(`b-${scope}-light-readonly.png`);
});

for(const scope of ['node','chain'])for(const width of [320,375,390,1200])test(`${scope} Premium title stays complete on one line ${width}`,async({page})=>{
 await page.clock.setFixedTime(new Date('2026-10-26T08:00:00Z'));await page.setViewportSize({width,height:1100});
 await page.goto(`/tests/fixtures/node-unlock-preview.html?scope=${scope}&scene=macau`);
 if(scope==='chain')await page.getByRole('button',{name:'解锁',exact:true}).click();
 const panel=scope==='chain'?page.getByRole('dialog',{name:/解锁/}):page.getByRole('tabpanel',{name:'解锁'});
 await expect(panel.getByRole('article')).toHaveCount(13);await expectPremiumTitle(panel);
 expect(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth)).toBe(false);
 await expect(page).toHaveScreenshot(`b-${scope}-wide-title-${width}.png`);
});
for(const width of [1440,390])test(`YouTube independent facts and nested focus ${width}`,async({page})=>{
  await page.clock.setFixedTime(new Date('2026-10-26T08:00:00Z'));await page.setViewportSize({width,height:1100});
  await page.goto('/tests/fixtures/node-unlock-preview.html?scope=chain&youtube-details=1');
  const open=page.getByRole('button',{name:'解锁',exact:true});await open.click();const chain=page.getByRole('dialog',{name:/解锁/});
  const more=chain.getByRole('button',{name:'YouTube 分项'});await more.click();const detail=page.getByRole('region',{name:'YouTube 检测详情'});
  await expect(detail.getByText('地区',{exact:true}).locator('..')).toContainText('MO');await expect(detail.getByText('页面配置地区').locator('..')).toContainText('US');await expect(detail.getByText('隐私元数据 · 地区').locator('..')).toContainText('MO');await expect(detail).toContainText('hkg12s15');
  expect(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth)).toBe(false);await expect(page).toHaveScreenshot(`b-youtube-${width}.png`);
  await page.keyboard.press('Escape');await expect(detail).toHaveCount(0);await expect(more).toBeFocused();await expect(chain).toBeVisible();
  await page.keyboard.press('Escape');await expect(chain).toHaveCount(0);await expect(open).toBeFocused();
});
test('YouTube partial failure keeps webpage and legacy details remain accessible',async({page})=>{
  await page.clock.setFixedTime(new Date('2026-10-26T08:00:00Z'));await page.setViewportSize({width:390,height:844});
  await page.goto('/tests/fixtures/node-unlock-preview.html?scope=chain&youtube-details=1&youtube-partial=1');await page.getByRole('button',{name:'解锁',exact:true}).click();await page.getByRole('button',{name:'YouTube 分项'}).click();
  const detail=page.getByRole('region',{name:'YouTube 检测详情'});await expect(detail.getByText('YouTube 网页').locator('..')).toContainText('可用');await expect(detail).toContainText('请求超时');await expect(page).toHaveScreenshot('b-youtube-partial.png');
});

for(const width of [1440,390])test(`previous failures are hidden at ${width}`,async({page})=>{
 await page.clock.setFixedTime(new Date('2026-10-26T08:00:00Z'));await page.setViewportSize({width,height:1100});
 await page.goto('/tests/fixtures/node-unlock-preview.html?scene=typical&previous-failed=1');const panel=page.getByRole('tabpanel',{name:'解锁'});
 await expect(panel.getByRole('article')).toHaveCount(13);await expect(panel.getByRole('article',{name:'Netflix'})).toContainText('HK');await expect(panel.getByText(/上次/)).toHaveCount(0);
 await expect(panel.getByRole('article',{name:'TikTok'})).toContainText('检测失败');await expect(page).toHaveScreenshot(`b-hide-previous-failed-${width}.png`);
});

for (const scope of ['node','chain']) for (const width of [1440,390]) test(`IPv6 independent result and manual retry ${scope} ${width}`,async({page})=>{
 await page.clock.setFixedTime(new Date('2026-10-26T08:00:00Z'));await page.setViewportSize({width,height:1100});
 await page.goto(`/tests/fixtures/node-unlock-preview.html?scope=${scope}&scene=macau`);
 if(scope==='chain')await page.getByRole('button',{name:'解锁',exact:true}).click();
 const panel=scope==='chain'?page.getByRole('dialog',{name:/解锁/}):page.getByRole('tabpanel',{name:'解锁'});
 await expect(panel.getByRole('article',{name:'YouTube',exact:true})).toContainText('视频 CDN HKG');
 await panel.getByRole('button',{name:'IPv6',exact:true}).click();
 const youtube=panel.getByRole('article',{name:'YouTube',exact:true});
 const footer=panel.locator('.unlock-footer');await expect(footer.getByRole('group',{name:'解锁地址族'})).toBeVisible();
 const choice=await footer.getByRole('group',{name:'解锁地址族'}).boundingBox();const footerBox=await footer.boundingBox();expect(choice!.x-footerBox!.x).toBeLessThanOrEqual(17);expect(choice!.y).toBeGreaterThanOrEqual(footerBox!.y);
 await expect(youtube).toContainText('视频 CDN NRT');await expect(youtube).toContainText('JP');await expect(youtube).not.toContainText('HKG');
 await expectPremiumTitle(panel);expect(await page.evaluate(()=>document.documentElement.scrollWidth>innerWidth)).toBe(false);
 await expect(page).toHaveScreenshot(`b-${scope}-ipv6-${width}.png`);
 const retry=scope==='node'?page.getByRole('button',{name:'重新检测',exact:true}):panel.getByRole('button',{name:'重新检测',exact:true});await retry.click();await expect(panel.getByText('等待机器响应',{exact:true})).toBeVisible();
 await panel.getByRole('button',{name:'IPv4',exact:true}).click();await expect(youtube).toContainText('视频 CDN HKG');
 await expect(panel.getByText('等待机器响应',{exact:true})).toHaveCount(0);await expect(retry).toBeEnabled();
});
for(const scope of ['node','chain'])test(`missing IPv6 never changes IPv4 ${scope}`,async({page})=>{
 await page.clock.setFixedTime(new Date('2026-10-26T08:00:00Z'));await page.setViewportSize({width:1200,height:1000});
 await page.goto(`/tests/fixtures/node-unlock-preview.html?scope=${scope}&scene=macau&no-ipv6=1`);if(scope==='chain')await page.getByRole('button',{name:'解锁',exact:true}).click();
 const panel=scope==='chain'?page.getByRole('dialog',{name:/解锁/}):page.getByRole('tabpanel',{name:'解锁'});
 await panel.getByRole('button',{name:'IPv6',exact:true}).click();await expect(panel.getByRole('article')).toHaveCount(13);
 await expect(panel.getByRole('article',{name:'Netflix',exact:true})).toContainText('检测失败 · 网络连接失败');
 await expect(panel.getByRole('article',{name:'Netflix',exact:true})).not.toContainText('地区不支持');
 await expect(panel.locator('.unlock-b-hero')).toHaveAttribute('aria-label','可用 0 / 13');await expect(page).toHaveScreenshot(`b-${scope}-no-ipv6.png`);
 await panel.getByRole('button',{name:'IPv4',exact:true}).click();await expect(panel.getByRole('article',{name:'YouTube',exact:true})).toContainText('视频 CDN HKG');
});
test('chain family capability is explicit before updated profiles are applied',async({page})=>{
 await page.clock.setFixedTime(new Date('2026-10-26T08:00:00Z'));await page.setViewportSize({width:1200,height:1000});
 await page.goto('/tests/fixtures/node-unlock-preview.html?scope=chain&scene=macau&family-unavailable=1');await page.getByRole('button',{name:'解锁',exact:true}).click();
 const panel=page.getByRole('dialog',{name:/解锁/});await expect(panel).toContainText('正在等待强制地址族的检测配置就绪，稍后自动重试');await expect(panel.getByRole('article')).toHaveCount(0);
 await expect(page).toHaveScreenshot('b-chain-family-unavailable.png');
});

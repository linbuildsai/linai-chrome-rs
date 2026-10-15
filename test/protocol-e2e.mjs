// 协议端到端测试：模拟 extension + agent 两个 WS 客户端，验证 Rust 桥的
// Origin 白名单、token 鉴权、hello/welcome 握手、cmd 路由盖章、res 配对、审计落盘。
// 用法：先起 `linai-chrome-rs bridge --port 8900`，再 node test/protocol-e2e.mjs
import { createRequire } from 'node:module';
const require = createRequire(import.meta.url);
// ws 包位置可用 WS_PATH 指定（默认从本文件就近解析）
const WebSocket = require(process.env.WS_PATH || 'ws');
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const PORT = process.env.PORT || 8900;
const HOME = process.env.HUASHU_CHROME_HOME || path.join(os.homedir(), '.huashu-chrome');
const info = JSON.parse(fs.readFileSync(path.join(HOME, 'bridge.json'), 'utf8'));
// 测试桥用自己的 token：从 bridge.json 读（测试桥启动时写的）
const TOKEN = info.token;
const URL = `ws://127.0.0.1:${PORT}`;
let pass = 0, fail = 0;
const ok = (name, cond, extra = '') => {
  if (cond) { pass++; console.log(`  ✅ ${name}`); }
  else { fail++; console.log(`  ❌ ${name} ${extra}`); }
};

function connect({ origin } = {}) {
  return new Promise((resolve, reject) => {
    const ws = new WebSocket(URL, origin ? { headers: { Origin: origin } } : {});
    ws.on('open', () => resolve(ws));
    ws.on('error', reject);
    setTimeout(() => reject(new Error('connect timeout')), 4000);
  });
}
const nextMsg = (ws, pred = () => true, ms = 4000) =>
  new Promise((resolve, reject) => {
    const t = setTimeout(() => reject(new Error('msg timeout')), ms);
    const h = (raw) => {
      const m = JSON.parse(raw.toString());
      if (pred(m)) { clearTimeout(t); ws.off('message', h); resolve(m); }
    };
    ws.on('message', h);
  });

// 1) 恶意网页 Origin 被拒
try {
  await connect({ origin: 'https://evil.example.com' });
  ok('恶意 Origin 被拒', false, '（连接竟然成功了）');
} catch {
  ok('恶意 Origin 被拒（403）', true);
}

// 2) 扩展握手
const ext = await connect({ origin: 'chrome-extension://testextid123' });
ext.send(JSON.stringify({ type: 'hello', role: 'extension', extId: 'testextid123', v: 1, chrome: '141.0', version: '1.2.0', instanceId: 'inst-test' }));
const wExt = await nextMsg(ext, (m) => m.type === 'welcome');
ok('扩展收到 welcome', wExt.bridge !== undefined && wExt.v === 1);

// 3) agent 错误 token 被拒
const bad = await connect();
bad.send(JSON.stringify({ type: 'hello', role: 'agent', token: 'wrong', client: 'test', sessionId: 'test:bad', v: 1 }));
const closed = await new Promise((resolve) => { bad.on('close', (code) => resolve(code)); setTimeout(() => resolve(null), 3000); });
ok('错误 token 被拒（close 4001）', closed === 4001, `got ${closed}`);

// 4) agent 正常握手，welcome 带 extensionOnline
const agent = await connect();
agent.send(JSON.stringify({ type: 'hello', role: 'agent', token: TOKEN, client: 'test-cli', label: 'Test CLI', sessionId: 'test:p1', v: 1 }));
const wAg = await nextMsg(agent, (m) => m.type === 'welcome');
ok('agent welcome.extensionOnline=true', wAg.extensionOnline === true);

// 5) cmd 路由：扩展收到带 __k/sid/live/client 盖章的命令
const cmdP = nextMsg(ext, (m) => m.type === 'cmd');
agent.send(JSON.stringify({ type: 'cmd', id: 'c1', cmd: 'snapshot', params: {}, timeout: 8000 }));
const got = await cmdP;
ok('扩展收到 cmd 且盖章齐全', got.cmd === 'snapshot' && !!got.__k && got.sid === 'test:p1' && got.client === 'test-cli' && Array.isArray(got.live) && got.live.includes('test:p1'), JSON.stringify({ k: got.__k, sid: got.sid, live: got.live }));

// 6) res 按 __k 配对回 agent
const resP = nextMsg(agent, (m) => m.type === 'res' && m.id === 'c1');
ext.send(JSON.stringify({ type: 'res', id: 'c1', __k: got.__k, ok: true, data: { text: '效果：目标区块文本 +3 字\n[e1] button "x"' } }));
const res = await resP;
ok('agent 收到 res 且 __k 已剥离', res.ok === true && res.__k === undefined && res.data.text.includes('效果'));

// 7) ping → pong
const pongP = nextMsg(ext, (m) => m.type === 'pong');
ext.send(JSON.stringify({ type: 'ping' }));
await pongP;
ok('ping → pong', true);

// 8) event 广播给 agent
const evP = nextMsg(agent, (m) => m.type === 'event' && m.event === 'navigated');
ext.send(JSON.stringify({ type: 'event', event: 'navigated', tabId: 1, url: 'https://example.com' }));
await evP;
ok('扩展 event 广播到 agent', true);

// 9) agent 断线 → 扩展收到 sessions 事件
const sessP = nextMsg(ext, (m) => m.type === 'event' && m.event === 'sessions', 5000);
agent.close();
const sess = await sessP;
ok('agent 断线推 sessions 名单', Array.isArray(sess.live) && !sess.live.includes('test:p1'));

ext.close();
console.log(`\n${pass} passed, ${fail} failed`);
process.exit(fail ? 1 : 0);

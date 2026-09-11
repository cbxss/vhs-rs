import test from "node:test";
import assert from "node:assert/strict";
import { mkdtemp, mkdir, cp, writeFile, readFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { pathToFileURL } from "node:url";
const installed = join(
  process.env.VHS_TEST_PACKAGE_ROOT,
  "node_modules/@cbxss",
);
const platform = `${process.platform === "darwin" ? "darwin" : "linux"}-${process.arch}`;

async function fixture(t, script, manifestVersion = "0.3.0") {
  const root = await mkdtemp(join(tmpdir(), "vhs-transport-"));
  t.after(() => rm(root, { recursive: true, force: true }));
  const scope = join(root, "node_modules/@cbxss");
  await mkdir(scope, { recursive: true });
  await cp(join(installed, "vhs-rs"), join(scope, "vhs-rs"), {
    recursive: true,
  });
  const binaryDir = join(scope, `vhs-rs-${platform}`);
  await mkdir(join(binaryDir, "bin"), { recursive: true });
  await writeFile(
    join(binaryDir, "package.json"),
    JSON.stringify({
      name: `@cbxss/vhs-rs-${platform}`,
      version: manifestVersion,
    }),
  );
  await writeFile(
    join(binaryDir, "bin/vhs-rs"),
    `#!${process.execPath}\n${script}\n`,
    { mode: 0o755 },
  );
  const sdk = await import(pathToFileURL(join(scope, "vhs-rs/dist/index.js")));
  return { sdk, root };
}
const hello = `console.log(JSON.stringify({kind:'ready',version:1,binary_version:'0.3.0'}));`;
const configure = `let buf=''; process.stdin.setEncoding('utf8'); process.stdin.on('data', chunk => { buf+=chunk; let end; while((end=buf.indexOf('\\n'))>=0) { const req=JSON.parse(buf.slice(0,end));buf=buf.slice(end+1); handle(req); } });`;

test("mismatched package and wire versions fail closed", async (t) => {
  const badPackage = await fixture(t, "", "9.9.9");
  await assert.rejects(
    badPackage.sdk.createSession(),
    (e) => e.reason === "version_mismatch",
  );
  const badWire = await fixture(
    t,
    `console.log('{"kind":"ready","version":99,"binary_version":"0.3.0"}'); setInterval(()=>{},1000);`,
  );
  await assert.rejects(
    badWire.sdk.createSession(),
    (e) => e.reason === "version_mismatch",
  );
});

test("malformed responses reject pending promises and terminate", async (t) => {
  const { sdk } = await fixture(
    t,
    hello +
      configure +
      `function handle(req) { if(req.command.op==='configure') console.log(JSON.stringify({kind:'result',id:req.id,status:'ok'})); else console.log(JSON.stringify({kind:'result',id:req.id,status:'failed',failure:{}})); }`,
  );
  const s = await sdk.createSession();
  await assert.rejects(s.screen(), (e) => e.reason === "protocol_error");
  await assert.rejects(s.close());
});

test("partial frames and split unicode are decoded across pipe chunks", async (t) => {
  const { sdk } = await fixture(
    t,
    hello +
      configure +
      `function handle(req) {
    if(req.command.op==='configure') console.log(JSON.stringify({kind:'result',id:req.id,status:'ok'}));
    else if(req.command.op==='screen') { const bytes=Buffer.from(JSON.stringify({kind:'result',id:req.id,status:'ok',detail:{screen_text:'雪',cursor:{col:0,row:0,visible:true}}})+'\\n'); const cut=bytes.indexOf(Buffer.from('雪'))+1; process.stdout.write(bytes.subarray(0,cut));setTimeout(()=>process.stdout.write(bytes.subarray(cut)),10); }
    else { console.log(JSON.stringify({kind:'result',id:req.id,status:'ok',report:{version:1,tape:'repl',status:'success',exit_code:0,commands:[],artifacts:[]}}));process.stdin.pause();process.exitCode=0; }
  }`,
  );
  const s = await sdk.createSession();
  assert.equal((await s.screen()).screen_text, "雪");
  await s.close();
});

test("abort force-kills an unresponsive child and rejects all outstanding work", async (t) => {
  const { sdk, root } = await fixture(
    t,
    hello +
      configure +
      `process.on('SIGTERM',()=>{}); function handle(req) { if(req.command.op==='configure') console.log(JSON.stringify({kind:'result',id:req.id,status:'ok'})); }`,
  );
  const controller = new AbortController();
  const s = await sdk.createSession({
    signal: controller.signal,
    shutdownTimeoutMs: 50,
  });
  const pending = Promise.allSettled([s.screen(), s.screen()]);
  controller.abort();
  assert.ok(
    (await pending).every(
      (result) =>
        result.status === "rejected" && result.reason.reason === "aborted",
    ),
  );
  await assert.rejects(s.close(), (e) => e.reason === "aborted");
});

test("close has a bounded finalization deadline", async (t) => {
  const { sdk } = await fixture(
    t,
    hello +
      configure +
      `process.on('SIGTERM',()=>{}); function handle(req) { if(req.command.op==='configure') console.log(JSON.stringify({kind:'result',id:req.id,status:'ok'})); }`,
  );
  const s = await sdk.createSession({ shutdownTimeoutMs: 50 });
  const close = s.close();
  assert.equal(close, s.close());
  await assert.rejects(close, (e) => e.reason === "shutdown_timeout");
});

test("batch rejects contradictory reports and oversized Node timers", async (t) => {
  const { sdk } = await fixture(
    t,
    `console.log(JSON.stringify({version:1,tape:'-',status:'success',exit_code:0,commands:[],artifacts:[]}));process.exitCode=4;`,
  );
  await assert.rejects(
    sdk.run({ tape: "" }),
    (e) => e.reason === "protocol_error",
  );
  await assert.rejects(
    sdk.run({ tape: "", timeoutMs: 0xffff_ffff }),
    (e) => e.reason === "invalid_argument",
  );
  await assert.rejects(sdk.run({}), (e) => e.reason === "invalid_argument");
});

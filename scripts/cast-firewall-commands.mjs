// Read the Cast firewall command lines out of src-tauri/windows/installer.nsi.
// That file is the only copy. NSIS `$\"` becomes a plain quote, `$$` becomes `$`,
// then ${defines}, $INSTDIR, and $SYSDIR are substituted the way the installer does.
//
//   node scripts/cast-firewall-commands.mjs --check
//   node scripts/cast-firewall-commands.mjs --apply
//
// --check proves the runtime text has plain inner quotes and no \" .
// --apply (Windows, already elevated) runs those exact command lines.

import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const nsiPath = path.join(root, 'src-tauri', 'windows', 'installer.nsi');
const confPath = path.join(root, 'src-tauri', 'tauri.conf.json');

function fail(message) {
  throw new Error(message);
}

function parseDefines(text) {
  const defs = {};
  for (const line of text.split(/\r?\n/)) {
    const match = line.match(/^\s*!define\s+([A-Za-z0-9_]+)\s+"(.*)"\s*$/);
    if (match) defs[match[1]] = match[2];
  }
  return defs;
}

function functionBody(text, header) {
  const start = text.indexOf(header);
  if (start < 0) fail(`missing ${header} in installer.nsi`);
  const end = text.indexOf('FunctionEnd', start);
  if (end < 0) fail(`missing FunctionEnd after ${header}`);
  return text.slice(start, end);
}

function extractQuoted(body, label) {
  const marker = "StrCpy $R9 '";
  const at = body.indexOf(marker);
  if (at < 0) fail(`missing ${marker} in ${label}`);
  const from = at + marker.length;
  const end = body.indexOf("'", from);
  if (end < 0) fail(`unclosed StrCpy in ${label}`);
  return body.slice(from, end);
}

function mainBinaryName() {
  const conf = JSON.parse(fs.readFileSync(confPath, 'utf8'));
  const name = conf.mainBinaryName;
  if (!name) fail('tauri.conf.json has no mainBinaryName');
  return name;
}

function nsisRuntime(raw, vars) {
  let out = '';
  for (let i = 0; i < raw.length; i++) {
    if (raw.startsWith('$\\"', i)) {
      out += '"';
      i += 2;
      continue;
    }
    out += raw[i];
  }
  if (out.includes('\\"')) fail(`runtime command still contains \\" : ${out}`);
  out = out.replace(/\$\{([A-Za-z0-9_]+)\}/g, (all, name) => {
    if (!Object.prototype.hasOwnProperty.call(vars, name)) fail(`undefined NSIS define \${${name}}`);
    return vars[name];
  });
  out = out.replaceAll('$INSTDIR', vars.INSTDIR);
  out = out.replaceAll('$SYSDIR', vars.SYSDIR);
  if (out.includes('${') || out.includes('$INSTDIR') || out.includes('$SYSDIR') || out.includes('$\\')) {
    fail(`unresolved NSIS token in: ${out}`);
  }
  out = out.replaceAll('$$', '$');
  return out;
}

function loadCommands(instDir, sysDir) {
  const text = fs.readFileSync(nsiPath, 'utf8');
  const defs = parseDefines(text);
  const binary = mainBinaryName();
  const definedBinary = defs.MAINBINARYNAME;
  if (definedBinary && !definedBinary.includes('{{') && definedBinary !== binary) {
    fail(`MAINBINARYNAME ${definedBinary} does not match tauri.conf.json ${binary}`);
  }
  const vars = {
    ...defs,
    MAINBINARYNAME: binary,
    INSTDIR: instDir,
    SYSDIR: sysDir,
  };
  const installParams = nsisRuntime(extractQuoted(functionBody(text, 'Function CastFirewallInstall'), 'install'), vars);
  const uninstallParams = nsisRuntime(
    extractQuoted(functionBody(text, 'Function un.CastFirewallUninstall'), 'uninstall'),
    vars,
  );
  const installBody = functionBody(text, 'Function CastFirewallInstall');
  const prechecks = [...installBody.matchAll(/nsExec::ExecToStack `([^`]*)`/g)].map((match) =>
    nsisRuntime(match[1], vars),
  );
  const matchAt = installBody.indexOf('status=match');
  const returnAt = installBody.indexOf('\n Return\n');
  const runasAt = installBody.indexOf('ExecShell "runas"');
  return {
    defs,
    binary,
    installParams,
    uninstallParams,
    prechecks,
    sysDir,
    instDir,
    skipsElevation: matchAt >= 0 && returnAt > matchAt && runasAt > returnAt,
  };
}

function assertPlainQuotes(label, params, instDir, binary) {
  if (!params.startsWith('/c "')) fail(`${label} does not start with /c "`);
  if (!params.endsWith('"')) fail(`${label} does not end with the outer quote`);
  if (params.includes('\\"')) fail(`${label} still has a backslash-quote`);
  const command = params.slice('/c "'.length, -1);
  if (command.startsWith('"')) {
    fail(`${label} has an extra quote after the /c opener: ${params}`);
  }
  const quotes = [...command].filter((ch) => ch === '"').length;
  if (quotes % 2 !== 0) fail(`${label} has unbalanced inner quotes (${quotes})`);
  const exe = `${instDir}\\${binary}.exe`;
  const result = `${instDir}\\cast-firewall.txt`;
  for (const piece of [
    'name="SlideX Cast media (Private)"',
    'name="SlideX Cast mDNS (Private)"',
    `program="${exe}"`,
  ]) {
    if (label === 'uninstall' && piece.startsWith('program=')) continue;
    if (!params.includes(piece)) fail(`${label} missing ${piece}`);
  }
  if (label === 'install') {
    if (!params.includes('localport=47200-47215')) fail('install missing TCP range');
    if (!params.includes('localport=5353')) fail('install missing UDP port');
    if (!params.includes('profile=private')) fail('install missing private profile');
    const localSubnet = params.split('remoteip=localsubnet').length - 1;
    if (localSubnet !== 2) fail(`install should set remoteip=localsubnet on both rules, found ${localSubnet}`);
    if (!params.includes(`(echo status=added>"${result}")`)) fail('install missing status=added redirect');
    if (!params.includes(`(echo status=failed>"${result}")`)) fail('install missing status=failed redirect');
  } else {
    if (!params.includes(`echo status=removed>"${result}"`)) fail('uninstall missing status=removed redirect');
  }
  return command;
}

function printCommand(label, sysDir, params) {
  const line = `${sysDir}\\cmd.exe ${params}`;
  console.log(`CAST_FIREWALL_${label}_CMDLINE=${line}`);
  return line;
}

function runCmd(sysDir, params) {
  const exe = path.join(sysDir, 'cmd.exe');
  const result = spawnSync(exe, [params], {
    windowsVerbatimArguments: true,
    stdio: 'inherit',
  });
  if (result.error) fail(result.error.message);
  if (result.status !== 0) fail(`cmd exited ${result.status}`);
}

function splitPrecheck(commandLine) {
  const match = commandLine.match(/^"([^"]+)"\s+-NoProfile\s+-NonInteractive\s+-Command\s+"([\s\S]*)"$/);
  if (!match) fail(`pre-check is not a full-path PowerShell -NoProfile -NonInteractive command: ${commandLine}`);
  const exe = match[1];
  if (!exe.toLowerCase().endsWith('\\system32\\windowspowershell\\v1.0\\powershell.exe')) {
    fail(`pre-check exe is not System32 PowerShell: ${exe}`);
  }
  return { exe, script: match[2] };
}

function runPrecheck(commandLine) {
  const { exe, script } = splitPrecheck(commandLine);
  const result = spawnSync(exe, ['-NoProfile', '-NonInteractive', '-Command', script], { encoding: 'utf8' });
  return {
    status: result.status,
    text: `${result.stdout || ''}\n${result.stderr || ''}`,
    error: result.error,
  };
}

function netshShow(sysDir, ruleName) {
  const exe = path.join(sysDir, 'netsh.exe');
  const result = spawnSync(
    exe,
    ['advfirewall', 'firewall', 'show', 'rule', `name=${ruleName}`, 'verbose'],
    { encoding: 'utf8' },
  );
  const text = `${result.stdout || ''}\n${result.stderr || ''}`;
  return { status: result.status, text };
}

function field(text, name) {
  const match = text.match(new RegExp(`^${name}:\\s*(.*)$`, 'im'));
  return match ? match[1].trim() : '';
}

function assertRule(text, ruleName, exePath, protocol, ports) {
  if (/no rules match/i.test(text)) fail(`rule missing: ${ruleName}\n${text}`);
  const gotName = field(text, 'Rule Name');
  if (gotName !== ruleName) fail(`rule name ${JSON.stringify(gotName)} != ${JSON.stringify(ruleName)}`);
  const profiles = field(text, 'Profiles');
  if (!/private/i.test(profiles) || /public/i.test(profiles)) {
    fail(`${ruleName} profile is ${JSON.stringify(profiles)}, want Private only`);
  }
  const proto = field(text, 'Protocol');
  if (proto.toUpperCase() !== protocol) fail(`${ruleName} protocol ${proto} != ${protocol}`);
  const localPort = field(text, 'LocalPort');
  if (localPort !== ports) fail(`${ruleName} port ${localPort} != ${ports}`);
  const program = field(text, 'Program');
  if (program.toLowerCase() !== exePath.toLowerCase()) {
    fail(`${ruleName} program ${program} != ${exePath}`);
  }
  const action = field(text, 'Action');
  if (!/allow/i.test(action)) fail(`${ruleName} action ${action}`);
  const remote = field(text, 'RemoteIP');
  if (!/local\s*subnet/i.test(remote)) {
    fail(`${ruleName} remote IP ${JSON.stringify(remote)}, want LocalSubnet`);
  }
}

function assertGone(text, ruleName) {
  if (!/no rules match/i.test(text)) fail(`rule still present: ${ruleName}\n${text}`);
}

function readStatus(file) {
  if (!fs.existsSync(file)) fail(`missing ${file}`);
  return fs.readFileSync(file, 'utf8');
}

// os.tmpdir() on the GitHub runner is an 8.3 path (C:\Users\RUNNER~1\...).
// netsh reports "The application name could not be resolved" for that form,
// and also for a zero-byte file. Use the long path and a real PE image.
function stripDevicePrefix(p) {
  if (p.startsWith('\\\\?\\UNC\\')) return `\\\\${p.slice('\\\\?\\UNC\\'.length)}`;
  if (p.startsWith('\\\\?\\')) return p.slice('\\\\?\\'.length);
  return p;
}

function prepareInstallDir() {
  const candidates = [];
  try {
    candidates.push(path.join(stripDevicePrefix(fs.realpathSync.native(os.tmpdir())), 'SlideX Cast CI'));
  } catch {
    // TEMP can be missing in a stripped environment. The drive root is the fallback.
  }
  candidates.push(`${process.env.SystemDrive || 'C:'}\\SlideX Cast CI`);
  const errors = [];
  for (const dir of candidates) {
    try {
      fs.rmSync(dir, { recursive: true, force: true });
      fs.mkdirSync(dir, { recursive: true });
      const resolved = stripDevicePrefix(fs.realpathSync.native(dir));
      if (!resolved.includes('~')) return resolved;
      errors.push(`${resolved} is still a short path`);
      fs.rmSync(resolved, { recursive: true, force: true });
    } catch (error) {
      errors.push(`${dir}: ${error.message}`);
    }
  }
  fail(`could not create an install dir netsh can resolve\n${errors.join('\n')}`);
}

const apply = process.argv.includes('--apply');
if (apply && process.platform !== 'win32') fail('--apply runs on Windows');
const sysDir = process.env.SystemRoot ? path.join(process.env.SystemRoot, 'System32') : 'C:\\Windows\\System32';
const instDir = apply ? prepareInstallDir() : 'C:\\SlideX Cast CI';

const loaded = loadCommands(instDir, sysDir);
assertPlainQuotes('install', loaded.installParams, instDir, loaded.binary);
assertPlainQuotes('uninstall', loaded.uninstallParams, instDir, loaded.binary);
if (!loaded.skipsElevation) {
  fail('CastFirewallInstall does not return before ExecShell when the pre-check reports status=match');
}
if (loaded.prechecks.length !== 1) fail(`expected 1 unelevated PowerShell pre-check, found ${loaded.prechecks.length}`);
const precheck = loaded.prechecks[0];
const psExe = `${sysDir}\\WindowsPowerShell\\v1.0\\powershell.exe`;
if (!precheck.startsWith(`"${psExe}"`)) fail(`pre-check does not use the full PowerShell path: ${precheck}`);
if (!precheck.includes(' -NoProfile ')) fail('pre-check missing -NoProfile');
if (!precheck.includes(' -NonInteractive ')) fail('pre-check missing -NonInteractive');
for (const piece of [
  'Get-NetFirewallRule',
  'Get-NetFirewallApplicationFilter',
  'Get-NetFirewallAddressFilter',
  'Get-NetFirewallPortFilter',
  'LocalSubnet',
  'status=match',
]) {
  if (!precheck.includes(piece)) fail(`pre-check missing ${piece}`);
}
if (/\bnetsh\b/i.test(precheck)) fail('pre-check still shells out to netsh');
if (/(^|\s)powershell\.exe(\s|$)/i.test(precheck.replace(psExe, ''))) {
  fail('pre-check also invokes powershell.exe from PATH');
}
console.log(`CAST_FIREWALL_PRECHECK_CMDLINE=${precheck}`);

printCommand('INSTALL', sysDir, loaded.installParams);
printCommand('UNINSTALL', sysDir, loaded.uninstallParams);

splitPrecheck(precheck);

if (!apply) {
  console.log('cast firewall command check ok');
  process.exit(0);
}

const exePath = path.join(instDir, `${loaded.binary}.exe`);
fs.copyFileSync(path.join(sysDir, 'cmd.exe'), exePath);
const statusFile = path.join(instDir, 'cast-firewall.txt');

try {
  runCmd(sysDir, loaded.installParams);
  const added = readStatus(statusFile);
  console.log(`CAST_FIREWALL_STATUS_AFTER_INSTALL=${JSON.stringify(added)}`);
  if (!added.includes('status=added')) fail(`cast-firewall.txt did not record status=added:\n${added}`);

  const mediaName = loaded.defs.CAST_FW_MEDIA_NAME;
  const mdnsName = loaded.defs.CAST_FW_MDNS_NAME;
  const media = netshShow(sysDir, mediaName);
  const mdns = netshShow(sysDir, mdnsName);
  console.log('--- show rule media verbose ---');
  console.log(media.text.trim());
  console.log('--- show rule mdns verbose ---');
  console.log(mdns.text.trim());
  assertRule(media.text, mediaName, exePath, 'TCP', loaded.defs.CAST_FW_TCP);
  assertRule(mdns.text, mdnsName, exePath, 'UDP', loaded.defs.CAST_FW_UDP);

  const second = runPrecheck(loaded.prechecks[0]);
  console.log('--- second install pre-check (no elevation) ---');
  console.log(second.text.trim());
  if (second.error) fail(second.error.message);
  if (!second.text.includes('status=match')) {
    fail(`second install would elevate; pre-check did not report status=match:\n${second.text}`);
  }
  console.log('CAST_FIREWALL_SECOND_INSTALL=skip-elevation');

  runCmd(sysDir, loaded.uninstallParams);
  const removed = readStatus(statusFile);
  console.log(`CAST_FIREWALL_STATUS_AFTER_UNINSTALL=${JSON.stringify(removed)}`);
  if (!removed.includes('status=removed')) fail(`cast-firewall.txt did not record status=removed:\n${removed}`);

  const mediaGone = netshShow(sysDir, mediaName);
  const mdnsGone = netshShow(sysDir, mdnsName);
  console.log('--- show rule media after uninstall ---');
  console.log(mediaGone.text.trim());
  console.log('--- show rule mdns after uninstall ---');
  console.log(mdnsGone.text.trim());
  assertGone(mediaGone.text, mediaName);
  assertGone(mdnsGone.text, mdnsName);
  console.log('cast firewall install and uninstall ok');
} finally {
  try {
    const cleanup = loadCommands(instDir, sysDir);
    runCmd(sysDir, cleanup.uninstallParams);
  } catch {
    // The uninstall command already ran, or netsh was never installed.
  }
  fs.rmSync(instDir, { recursive: true, force: true });
}

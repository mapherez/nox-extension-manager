#!/usr/bin/env node
// Repairs metadata on an existing published release. Does not build or create releases.
import { execFileSync } from 'node:child_process';

const repository = process.env.GITHUB_REPOSITORY || 'mapherez/nox-extension-manager';
let token = process.env.GITHUB_TOKEN || process.env.GH_TOKEN;
if (!token) {
  try {
    const credential = execFileSync('git', ['credential', 'fill'], {
      input: 'protocol=https\nhost=github.com\n\n', encoding: 'utf8',
      env: { ...process.env, GIT_TERMINAL_PROMPT: '0', GCM_INTERACTIVE: 'never' },
      stdio: ['pipe', 'pipe', 'ignore'],
    });
    token = credential.split(/\r?\n/).find((line) => line.startsWith('password='))?.slice(9);
  } catch { /* An explicit token can be supplied by the release maintainer. */ }
}
if (!token) throw new Error('No GitHub release credentials available; no remote changes made.');
const headers = { Authorization: `Bearer ${token}`, Accept: 'application/vnd.github+json', 'X-GitHub-Api-Version': '2022-11-28' };
async function api(path, options = {}) {
  const response = await fetch(`https://api.github.com/repos/${repository}${path}`, { ...options, headers: { ...headers, ...options.headers } });
  if (!response.ok) throw new Error(`GitHub ${options.method || 'GET'} failed: ${response.status}`);
  return response.status === 204 ? null : response.json();
}
const release = await api('/releases/latest');
if (release.draft || release.prerelease) throw new Error('Expected the latest published stable release.');
const asset = release.assets.find((item) => item.name === 'latest.json');
if (!asset) throw new Error('Existing latest.json asset not found; nothing changed.');
const response = await fetch(asset.browser_download_url, { headers: { 'Cache-Control': 'no-cache' } });
if (!response.ok) throw new Error(`Manifest download failed: ${response.status}`);
const original = await response.text();
const manifest = JSON.parse(original);
if (manifest.version !== release.tag_name.replace(/^v/, '')) throw new Error('Manifest version differs from its release tag.');
let changed = false;
for (const platform of Object.values(manifest.platforms)) {
  const name = decodeURIComponent(new URL(platform.url).pathname.split('/').at(-1));
  const installer = release.assets.find((item) => item.name === name);
  const signature = release.assets.find((item) => item.name === `${name}.sig`);
  if (!installer || !signature) throw new Error(`Installer/signature missing for ${name}; nothing changed.`);
  const sigResponse = await fetch(signature.browser_download_url);
  if (!sigResponse.ok || (await sigResponse.text()).trim() !== platform.signature.trim()) throw new Error('Installer signature does not match the existing manifest.');
  const head = await fetch(installer.browser_download_url, { method: 'HEAD', redirect: 'follow' });
  if (!head.ok) throw new Error(`Published installer returned ${head.status}; nothing changed.`);
  changed ||= platform.url !== installer.browser_download_url;
  platform.url = installer.browser_download_url;
}
if (!changed) {
  console.log(`${release.tag_name}: manifest already correct; installer links verified.`);
} else {
  const uploadUrl = new URL(release.upload_url.replace(/\{.*$/, ''));
  uploadUrl.searchParams.set('name', 'latest.json');
  async function upload(body) {
    const result = await fetch(uploadUrl, { method: 'POST', headers: { ...headers, 'Content-Type': 'application/json' }, body });
    if (!result.ok) throw new Error(`Manifest upload failed: ${result.status}`);
    return result.json();
  }
  // Check write access before removing the old asset. Installers and signatures are untouched.
  const repo = await api('');
  if (!repo.permissions?.push) throw new Error('GitHub credentials lack write access; nothing changed.');
  await api(`/releases/assets/${asset.id}`, { method: 'DELETE' });
  try { await upload(`${JSON.stringify(manifest, null, 2)}\n`); }
  catch (error) {
    try { await upload(original); }
    catch { throw new Error(`${error.message}; failed to restore the original manifest. Re-upload latest.json manually.`); }
    throw error;
  }
  const verified = await fetch(`${asset.browser_download_url}?nox=${Date.now()}`, { headers: { 'Cache-Control': 'no-cache' } });
  if (!verified.ok) throw new Error(`Manifest replaced, but public verification returned ${verified.status}.`);
  const published = await verified.json();
  if (JSON.stringify(published.platforms) !== JSON.stringify(manifest.platforms)) throw new Error('Public manifest still differs; GitHub may be serving a cached asset.');
  console.log(`${release.tag_name}: latest.json repaired and public installer links verified. No release or installer was created.`);
}

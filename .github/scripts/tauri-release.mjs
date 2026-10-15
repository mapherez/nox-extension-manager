#!/usr/bin/env node

import { createReadStream } from 'node:fs';
import { mkdir, readFile, readdir, stat, writeFile } from 'node:fs/promises';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';

const appName = 'NoX Extension Manager';
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');
const args = parseArgs(process.argv.slice(2));

if (args.help) {
  printHelp();
  process.exit(0);
}

const packageJson = await readJson(path.join(rootDir, 'package.json'));
const tauriConfig = await readJson(path.join(rootDir, 'src-tauri', 'tauri.conf.json'));

const version = normalizeVersion(
  args.version ?? envValue('RELEASE_VERSION') ?? tauriConfig.version ?? packageJson.version,
);
const refName = envValue('GITHUB_REF_NAME');
const defaultTag = refName && /^v?\d+\.\d+\.\d+/.test(refName) ? refName : `v${version}`;
const tagName = args.tag ?? envValue('RELEASE_TAG') ?? defaultTag;
const repository = args.repo ?? envValue('GITHUB_REPOSITORY');
const token = envValue('GITHUB_TOKEN') ?? envValue('GH_TOKEN');
const artifactDir = path.resolve(
  rootDir,
  args['artifact-dir'] ??
    envValue('TAURI_RELEASE_ARTIFACT_DIR') ??
    path.join('src-tauri', 'target', 'release', 'bundle'),
);
const outputDir = path.resolve(
  rootDir,
  args['output-dir'] ?? envValue('TAURI_RELEASE_OUTPUT_DIR') ?? artifactDir,
);
const dryRun = isTruthy(args['dry-run']) || isTruthy(process.env.DRY_RUN);
const skipUpload =
  isTruthy(args['skip-upload']) ||
  isTruthy(args['no-upload']) ||
  isTruthy(process.env.TAURI_RELEASE_SKIP_UPLOAD);
const draft = isTruthy(process.env.RELEASE_DRAFT);
const prerelease = isTruthy(process.env.RELEASE_PRERELEASE);
const releaseName = envValue('RELEASE_NAME') ?? `${appName} ${version}`;
const releaseNotes = envValue('TAURI_RELEASE_NOTES') ?? envValue('RELEASE_NOTES') ?? '';
const pubDate = envValue('TAURI_RELEASE_PUB_DATE') ?? new Date().toISOString();

if (!repository) {
  fail('Missing GitHub repository. Set GITHUB_REPOSITORY or pass --repo owner/name.');
}

const candidates = await findUpdaterCandidates(artifactDir);
if (candidates.length === 0) {
  fail(
    `No signed Tauri updater artifacts were found under ${artifactDir}. ` +
      'Run `npm run tauri:build` with TAURI_SIGNING_PRIVATE_KEY configured first.',
  );
}

const selected = selectPlatformAssets(candidates);
if (selected.length === 0) {
  fail(
    'Signed artifacts were found, but none matched a supported updater platform. ' +
      'Expected Windows, macOS, or Linux Tauri updater artifacts.',
  );
}

await mkdir(outputDir, { recursive: true });

const latestJsonPath = path.join(outputDir, 'latest.json');
const artifactAssets = uniqueByPath(
  candidates.flatMap((candidate) => [candidate.assetPath, candidate.signaturePath]),
);
const uploadAssets = [...artifactAssets, latestJsonPath];
const uploadNames = new Map();
for (const assetPath of uploadAssets) {
  const name = releaseAssetName(assetPath);
  if (uploadNames.has(name)) {
    fail(`Release asset name collision: ${uploadNames.get(name)} and ${assetPath} both use ${name}.`);
  }
  uploadNames.set(name, assetPath);
}

// Read signatures before changing a release. Preview mode can only predict
// URLs; a real publication must use the metadata returned by GitHub.
const signatures = new Map(
  await Promise.all(
    selected.map(async (candidate) => [candidate.platform, await readSignature(candidate.signaturePath)]),
  ),
);

if (dryRun || skipUpload) {
  await writeLatestJson();
  printSummary({ artifactDir, latestJsonPath, repository, tagName, version, selected, uploadAssets });
  console.log('Preview URLs use the requested upload names; no GitHub upload URLs were resolved.');
  console.log(dryRun ? 'Dry run enabled; no assets were uploaded.' : 'Upload skipped.');
  process.exit(0);
}

if (!token) {
  fail('Missing GITHUB_TOKEN or GH_TOKEN. The script cannot upload release assets.');
}

const release = await getOrCreateRelease({
  repository,
  tagName,
  token,
  releaseName,
  releaseNotes,
  draft,
  prerelease,
});

const uploadedAssets = new Map();
for (const assetPath of artifactAssets) {
  const uploaded = await uploadReleaseAsset({
    repository,
    token,
    release,
    assetPath,
    assetName: releaseAssetName(assetPath),
  });
  uploadedAssets.set(path.resolve(assetPath), uploaded);
}

// Publish the manifest last, only after every installer and signature exists.
if (!release.draft) {
  for (const candidate of selected) {
    const asset = uploadedAssets.get(path.resolve(candidate.assetPath));
    const url = releaseAssetUrl(repository, tagName, asset.name);
    const response = await fetch(url, { method: 'HEAD', redirect: 'follow' });
    if (!response.ok) {
      fail(`Published installer URL returned ${response.status}: ${url}; latest.json was not published.`);
    }
  }
}
await writeLatestJson(uploadedAssets);
await uploadReleaseAsset({
  repository,
  token,
  release,
  assetPath: latestJsonPath,
  assetName: 'latest.json',
});

printSummary({ artifactDir, latestJsonPath, repository, tagName, version, selected, uploadAssets });
console.log(`Release ${tagName} is ready: https://github.com/${repository}/releases/tag/${tagName}`);

async function writeLatestJson(uploadedAssets) {
  const platforms = Object.fromEntries(selected.map((candidate) => {
    const uploaded = uploadedAssets?.get(path.resolve(candidate.assetPath));
    if (uploadedAssets && !uploaded?.name) {
      fail(`GitHub did not return the asset name for ${candidate.uploadName}; latest.json was not published.`);
    }
    return [candidate.platform, {
      signature: signatures.get(candidate.platform),
      // Draft uploads may return an untagged-* URL which expires on publish.
      // Use GitHub's actual asset name with the final release tag instead.
      url: releaseAssetUrl(repository, tagName, uploadedAssets ? uploaded.name : candidate.uploadName),
    }];
  }));
  const latestJson = { version, notes: releaseNotes, pub_date: pubDate, platforms };
  await writeFile(latestJsonPath, `${JSON.stringify(latestJson, null, 2)}\n`, 'utf8');
}

async function findUpdaterCandidates(directory) {
  const signatures = await findFiles(directory, (filePath) => filePath.endsWith('.sig'));
  const candidates = [];

  for (const signaturePath of signatures) {
    const assetPath = signaturePath.slice(0, -'.sig'.length);
    if (path.basename(assetPath) === 'latest.json') {
      continue;
    }

    if (!(await exists(assetPath))) {
      continue;
    }

    const platform = inferPlatform(assetPath);
    if (!platform) {
      continue;
    }

    candidates.push({
      assetPath,
      signaturePath,
      platform,
      uploadName: releaseAssetName(assetPath),
      priority: platformPriority(assetPath),
    });
  }

  return candidates.sort((left, right) => {
    if (left.platform !== right.platform) {
      return left.platform.localeCompare(right.platform);
    }
    return right.priority - left.priority || left.assetPath.localeCompare(right.assetPath);
  });
}

function selectPlatformAssets(candidates) {
  const byPlatform = new Map();

  for (const candidate of candidates) {
    const current = byPlatform.get(candidate.platform);
    if (!current || candidate.priority > current.priority) {
      byPlatform.set(candidate.platform, candidate);
    }
  }

  return [...byPlatform.values()].sort((left, right) =>
    left.platform.localeCompare(right.platform),
  );
}

function inferPlatform(filePath) {
  const normalized = filePath.replace(/\\/g, '/').toLowerCase();
  const fileName = path.basename(normalized);
  const arch =
    normalized.includes('aarch64') ||
    normalized.includes('arm64') ||
    normalized.includes('aarch64-apple') ||
    normalized.includes('aarch64-pc')
      ? 'aarch64'
      : 'x86_64';

  if (
    normalized.includes('/windows') ||
    normalized.includes('/msi/') ||
    normalized.includes('/nsis/') ||
    fileName.endsWith('.msi') ||
    fileName.endsWith('.msi.zip') ||
    fileName.endsWith('.exe') ||
    fileName.endsWith('.exe.zip') ||
    fileName.includes('setup')
  ) {
    return `windows-${arch}`;
  }

  if (
    normalized.includes('/darwin') ||
    normalized.includes('/macos') ||
    normalized.includes('/macos/') ||
    fileName.endsWith('.app.tar.gz') ||
    fileName.endsWith('.dmg') ||
    fileName.endsWith('.dmg.zip')
  ) {
    return `darwin-${arch}`;
  }

  if (
    normalized.includes('/linux') ||
    normalized.includes('/appimage/') ||
    normalized.includes('/deb/') ||
    normalized.includes('/rpm/') ||
    fileName.endsWith('.appimage') ||
    fileName.endsWith('.appimage.tar.gz') ||
    fileName.endsWith('.deb') ||
    fileName.endsWith('.rpm')
  ) {
    return `linux-${arch}`;
  }

  return null;
}

function platformPriority(filePath) {
  const normalized = filePath.replace(/\\/g, '/').toLowerCase();

  if (normalized.endsWith('.app.tar.gz')) return 100;
  if (normalized.endsWith('.appimage.tar.gz')) return 100;
  if (normalized.endsWith('.exe.zip')) return 95;
  if (normalized.endsWith('.msi.zip')) return 90;
  if (normalized.endsWith('.exe')) return 85;
  if (normalized.endsWith('.msi')) return 80;
  if (normalized.endsWith('.dmg')) return 70;
  if (normalized.endsWith('.appimage')) return 70;
  if (normalized.endsWith('.deb')) return 60;
  if (normalized.endsWith('.rpm')) return 50;

  return 1;
}

async function findFiles(directory, predicate) {
  const files = [];

  async function walk(currentDir) {
    let entries;
    try {
      entries = await readdir(currentDir, { withFileTypes: true });
    } catch (error) {
      if (error.code === 'ENOENT') return;
      throw error;
    }

    for (const entry of entries) {
      const entryPath = path.join(currentDir, entry.name);
      if (entry.isDirectory()) {
        await walk(entryPath);
      } else if (entry.isFile() && predicate(entryPath)) {
        files.push(entryPath);
      }
    }
  }

  await walk(directory);
  return files;
}

async function getOrCreateRelease({
  repository,
  tagName,
  token,
  releaseName,
  releaseNotes,
  draft,
  prerelease,
}) {
  const existing = await githubRequest({
    token,
    method: 'GET',
    pathName: `/repos/${repository}/releases/tags/${encodeURIComponent(tagName)}`,
    allowNotFound: true,
  });

  if (existing) {
    console.log(`Using existing GitHub release ${tagName}.`);
    return existing;
  }

  console.log(`Creating GitHub release ${tagName}.`);
  return githubRequest({
    token,
    method: 'POST',
    pathName: `/repos/${repository}/releases`,
    body: {
      tag_name: tagName,
      name: releaseName,
      body: releaseNotes,
      target_commitish: envValue('GITHUB_SHA'),
      draft,
      prerelease,
      make_latest: prerelease ? 'false' : 'true',
    },
  });
}

async function uploadReleaseAsset({ repository, token, release, assetPath, assetName }) {
  const existing = await findReleaseAsset({ repository, token, release, assetName });
  if (existing) {
    console.log(`Replacing existing release asset ${assetName}.`);
    await githubRequest({
      token,
      method: 'DELETE',
      pathName: `/repos/${repository}/releases/assets/${existing.id}`,
    });
  } else {
    console.log(`Uploading release asset ${assetName}.`);
  }

  const uploadBase = release.upload_url.replace(/\{.*$/, '');
  const url = new URL(uploadBase);
  url.searchParams.set('name', assetName);

  const fileInfo = await stat(assetPath);
  const response = await fetch(url, {
    method: 'POST',
    headers: {
      Accept: 'application/vnd.github+json',
      Authorization: `Bearer ${token}`,
      'Content-Length': String(fileInfo.size),
      'Content-Type': 'application/octet-stream',
      'X-GitHub-Api-Version': '2022-11-28',
    },
    body: createReadStream(assetPath),
    duplex: 'half',
  });

  if (!response.ok) {
    const text = await response.text();
    fail(`GitHub upload failed for ${assetName}: ${response.status} ${text}`);
  }

  const uploaded = await response.json();
  if (uploaded.state !== 'uploaded' || uploaded.size !== fileInfo.size || !uploaded.name) {
    fail(`GitHub did not confirm a complete upload for ${assetName}; latest.json was not published.`);
  }
  return uploaded;
}

async function findReleaseAsset({ repository, token, release, assetName }) {
  let page = 1;
  while (true) {
    const assets = await githubRequest({
      token,
      method: 'GET',
      pathName: `/repos/${repository}/releases/${release.id}/assets?per_page=100&page=${page}`,
    });

    const match = assets.find((asset) => asset.name === assetName);
    if (match) return match;
    if (assets.length < 100) return null;
    page += 1;
  }
}

async function githubRequest({ token, method, pathName, body, allowNotFound = false }) {
  const response = await fetch(`https://api.github.com${pathName}`, {
    method,
    headers: {
      Accept: 'application/vnd.github+json',
      Authorization: `Bearer ${token}`,
      'Content-Type': 'application/json',
      'X-GitHub-Api-Version': '2022-11-28',
    },
    body: body ? JSON.stringify(body) : undefined,
  });

  if (allowNotFound && response.status === 404) {
    return null;
  }

  if (!response.ok) {
    const text = await response.text();
    fail(`GitHub API request failed: ${method} ${pathName} -> ${response.status} ${text}`);
  }

  if (response.status === 204) {
    return null;
  }

  return response.json();
}

function releaseAssetUrl(repository, tagName, assetName) {
  return `https://github.com/${repository}/releases/download/${encodeURIComponent(tagName)}/${encodeURIComponent(assetName)}`;
}

function releaseAssetName(filePath) {
  // Request stable, URL-safe names so a retry also finds previously uploaded
  // assets instead of looking for a local filename which GitHub renamed.
  return path.basename(filePath).replace(/[^A-Za-z0-9._-]/g, '.');
}

async function readJson(filePath) {
  return JSON.parse(await readFile(filePath, 'utf8'));
}

async function readSignature(filePath) {
  return (await readFile(filePath, 'utf8')).trim();
}

async function exists(filePath) {
  try {
    await stat(filePath);
    return true;
  } catch (error) {
    if (error.code === 'ENOENT') return false;
    throw error;
  }
}

function normalizeVersion(value) {
  const version = String(value ?? '').trim().replace(/^v/i, '');
  if (!isSemver(version)) {
    fail(`Invalid or missing semantic version: ${value ?? ''}`);
  }

  return version;
}

function isSemver(value) {
  return /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/.test(
    value,
  );
}

function parseArgs(argv) {
  const parsed = {};
  const valueOptions = new Set(['artifact-dir', 'output-dir', 'repo', 'tag', 'version']);
  const flagOptions = new Set(['dry-run', 'skip-upload', 'no-upload', 'help']);

  for (let index = 0; index < argv.length; index += 1) {
    const token = argv[index];
    if (!token.startsWith('--')) {
      fail(`Unexpected argument: ${token}`);
    }

    const [rawKey, inlineValue] = token.slice(2).split('=', 2);
    if (!valueOptions.has(rawKey) && !flagOptions.has(rawKey)) {
      fail(`Unknown option: --${rawKey}`);
    }

    if (flagOptions.has(rawKey)) {
      if (inlineValue !== undefined) {
        fail(`Option --${rawKey} does not accept a value.`);
      }
      parsed[rawKey] = true;
      continue;
    }

    const nextToken = argv[index + 1];
    if (inlineValue !== undefined) {
      if (inlineValue === '') fail(`Missing value for --${rawKey}.`);
      parsed[rawKey] = inlineValue;
    } else if (nextToken && !nextToken.startsWith('--')) {
      parsed[rawKey] = nextToken;
      index += 1;
    } else {
      fail(`Missing value for --${rawKey}.`);
    }
  }

  return parsed;
}

function isTruthy(value) {
  return value === true || ['1', 'true', 'yes', 'on'].includes(String(value).toLowerCase());
}

function envValue(name) {
  const value = process.env[name];
  if (value === undefined || value.trim() === '') {
    return undefined;
  }

  return value.trim();
}

function uniqueByPath(paths) {
  return [...new Map(paths.map((filePath) => [path.resolve(filePath), filePath])).values()];
}

function printSummary({
  artifactDir,
  latestJsonPath,
  repository,
  tagName,
  version,
  selected,
  uploadAssets,
}) {
  console.log('Tauri release metadata created.');
  console.log(`Repository: ${repository}`);
  console.log(`Tag: ${tagName}`);
  console.log(`Version: ${version}`);
  console.log(`Artifact directory: ${artifactDir}`);
  console.log(`latest.json: ${latestJsonPath}`);
  console.log('Updater platforms:');
  for (const candidate of selected) {
    console.log(`- ${candidate.platform}: ${candidate.uploadName}`);
  }
  console.log('Release assets:');
  for (const assetPath of uploadAssets) {
    console.log(`- ${assetPath}`);
  }
}

function printHelp() {
  console.log(`
Usage: node .github/scripts/tauri-release.mjs [options]

Creates latest.json for the Tauri updater and optionally uploads the release
assets to GitHub Releases.

Options:
  --artifact-dir <path>   Directory containing Tauri bundle artifacts.
                          Defaults to src-tauri/target/release/bundle.
  --output-dir <path>     Directory where latest.json is written.
                          Defaults to the artifact directory.
  --repo <owner/name>     GitHub repository. Defaults to GITHUB_REPOSITORY.
  --tag <tag>             Release tag. Defaults to RELEASE_TAG or v<version>.
  --version <version>     App version. Defaults to tauri.conf.json version.
  --dry-run               Create latest.json but do not upload assets.
  --skip-upload           Create latest.json without uploading assets.
  --help                  Show this message.

Required for upload:
  GITHUB_TOKEN or GH_TOKEN

Optional environment:
  TAURI_RELEASE_NOTES
  RELEASE_DRAFT=true
  RELEASE_PRERELEASE=true
`);
}

function fail(message) {
  console.error(message);
  process.exit(1);
}

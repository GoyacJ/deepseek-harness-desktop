import { readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import process from "node:process";

const dshVersion = required("DSH_VERSION");
const minDesktop = required("DSH_MIN_DESKTOP");
const nodeRange = process.env.DSH_NODE_RANGE ?? "^22.19.0 || >=24.0.0";
const notes = process.env.DSH_COMPAT_NOTES ?? "";
const bundled = process.env.DSH_BUNDLED_VERSION ?? "0.1.0-rc.6";
const repository = process.env.GITHUB_REPOSITORY ?? "GoyacJ/deepseek-harness-desktop";
const checksumDirectory = required("DSH_CHECKSUM_DIR");
const outputPath = process.env.DSH_COMPAT_OUTPUT ?? "dsh-compat.json";
const existingPath = process.env.DSH_COMPAT_EXISTING ?? outputPath;
const platforms = ["darwin-arm64", "darwin-x64", "linux-x64", "win32-x64"];

function required(name) {
  const value = process.env[name];
  if (!value) {
    throw new Error(`${name} is required`);
  }
  return value;
}

async function readChecksum(platform) {
  const filename = `dsh-${dshVersion}-${platform}.tar.gz.sha256`;
  const contents = (await readFile(path.join(checksumDirectory, filename), "utf8")).trim();
  const sha256 = contents.split(/\s+/)[0];
  if (!/^[a-fA-F0-9]{64}$/.test(sha256)) {
    throw new Error(`Invalid checksum in ${filename}`);
  }
  return sha256.toLowerCase();
}

let manifest = {
  schemaVersion: 1,
  bundled,
  releases: [],
};

try {
  manifest = JSON.parse(await readFile(existingPath, "utf8"));
} catch (error) {
  if (error.code !== "ENOENT") {
    throw error;
  }
}

manifest.schemaVersion = 1;
manifest.bundled = bundled;
manifest.releases = Array.isArray(manifest.releases) ? manifest.releases : [];

const archives = {};
for (const platform of platforms) {
  archives[platform] = {
    url: `https://github.com/${repository}/releases/download/dsh-${dshVersion}/dsh-${dshVersion}-${platform}.tar.gz`,
    sha256: await readChecksum(platform),
  };
}

const release = {
  version: dshVersion,
  minDesktop,
  node: nodeRange,
  notes,
  archives,
};

const index = manifest.releases.findIndex((item) => item.version === dshVersion);
if (index >= 0) {
  manifest.releases[index] = release;
} else {
  manifest.releases.push(release);
}

manifest.releases.sort((left, right) => left.version.localeCompare(right.version, "en"));

await writeFile(outputPath, `${JSON.stringify(manifest, null, 2)}\n`);
console.log(`Wrote ${outputPath} with DSH ${dshVersion}`);

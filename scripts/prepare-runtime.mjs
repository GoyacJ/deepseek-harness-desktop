import { createHash } from "node:crypto";
import { mkdir, readFile, rm, chmod, copyFile, writeFile } from "node:fs/promises";
import { spawn } from "node:child_process";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const NODE_VERSION = "22.23.2";
const DSH_VERSION = "0.1.0-rc.6";
const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const projectRoot = path.resolve(scriptDirectory, "..");
const sourceRoot = path.join(projectRoot, "runtime");
const runtimeRoot = path.join(projectRoot, "src-tauri", "resources", "dsh-runtime");
const appRoot = path.join(runtimeRoot, "app");
const temporaryRoot = path.join(runtimeRoot, ".prepare");

const targets = {
  "darwin-arm64": {
    archive: `node-v${NODE_VERSION}-darwin-arm64.tar.gz`,
    executable: ["bin", "node"],
  },
  "darwin-x64": {
    archive: `node-v${NODE_VERSION}-darwin-x64.tar.gz`,
    executable: ["bin", "node"],
  },
  "linux-x64": {
    archive: `node-v${NODE_VERSION}-linux-x64.tar.xz`,
    executable: ["bin", "node"],
  },
  "win32-x64": {
    archive: `node-v${NODE_VERSION}-win-x64.zip`,
    executable: ["node.exe"],
  },
};

const targetKey = `${process.platform}-${process.arch}`;
const target = targets[targetKey];
if (!target) {
  throw new Error(`Unsupported release target: ${targetKey}`);
}

async function download(url, destination) {
  const response = await fetch(url, { redirect: "follow" });
  if (!response.ok) {
    throw new Error(`Download failed (${response.status}): ${url}`);
  }
  const bytes = Buffer.from(await response.arrayBuffer());
  await writeFile(destination, bytes);
  return bytes;
}

function run(program, commandArguments, options = {}) {
  return new Promise((resolve, reject) => {
    const child = spawn(program, commandArguments, { stdio: "inherit", ...options });
    child.once("error", reject);
    child.once("exit", (code, signal) => {
      if (code === 0) {
        resolve();
      } else {
        reject(new Error(`${program} failed: ${signal ?? `exit ${code}`}`));
      }
    });
  });
}

async function installNode() {
  const distributionUrl = `https://nodejs.org/dist/v${NODE_VERSION}`;
  const archivePath = path.join(temporaryRoot, target.archive);
  const checksums = await download(
    `${distributionUrl}/SHASUMS256.txt`,
    path.join(temporaryRoot, "SHASUMS256.txt"),
  );
  const archive = await download(`${distributionUrl}/${target.archive}`, archivePath);
  const expectedLine = checksums
    .toString("utf8")
    .split(/\r?\n/)
    .find((line) => line.endsWith(`  ${target.archive}`));
  if (!expectedLine) {
    throw new Error(`No checksum published for ${target.archive}`);
  }
  const expected = expectedLine.split(/\s+/)[0];
  const actual = createHash("sha256").update(archive).digest("hex");
  if (actual !== expected) {
    throw new Error(`Checksum mismatch for ${target.archive}`);
  }

  const extractRoot = path.join(temporaryRoot, "node");
  await mkdir(extractRoot, { recursive: true });
  if (process.platform === "win32") {
    await run(
      "powershell.exe",
      [
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "Expand-Archive -LiteralPath $env:DSH_NODE_ARCHIVE -DestinationPath $env:DSH_NODE_EXTRACT -Force",
      ],
      {
        env: {
          ...process.env,
          DSH_NODE_ARCHIVE: archivePath,
          DSH_NODE_EXTRACT: extractRoot,
        },
      },
    );
  } else {
    await run("tar", ["-xf", archivePath, "-C", extractRoot]);
  }

  const unpackedRoot = path.join(extractRoot, target.archive.replace(/\.(tar\.gz|tar\.xz|zip)$/, ""));
  const nodeRoot = path.join(runtimeRoot, "node");
  const destination =
    process.platform === "win32"
      ? path.join(nodeRoot, "node.exe")
      : path.join(nodeRoot, "bin", "node");
  await mkdir(path.dirname(destination), { recursive: true });
  await copyFile(path.join(unpackedRoot, ...target.executable), destination);
  await copyFile(path.join(unpackedRoot, "LICENSE"), path.join(nodeRoot, "LICENSE"));
  if (process.platform !== "win32") {
    await chmod(destination, 0o755);
  }
}

async function installDsh() {
  await mkdir(appRoot, { recursive: true });
  for (const filename of ["package.json", "package-lock.json"]) {
    await copyFile(path.join(sourceRoot, filename), path.join(appRoot, filename));
  }
  const npm = process.platform === "win32" ? "npm.cmd" : "npm";
  await run(
    npm,
    [
      "ci",
      "--omit=dev",
      "--no-audit",
      "--no-fund",
      "--registry=https://registry.npmjs.org",
    ],
    { cwd: appRoot },
  );

  const installed = JSON.parse(
    await readFile(path.join(appRoot, "node_modules", "@deepseek-ai", "dsh", "package.json"), "utf8"),
  );
  if (installed.version !== DSH_VERSION) {
    throw new Error(`Expected DSH ${DSH_VERSION}, installed ${installed.version}`);
  }
}

await rm(runtimeRoot, { recursive: true, force: true });
await mkdir(temporaryRoot, { recursive: true });
await Promise.all([installNode(), installDsh()]);
await rm(temporaryRoot, { recursive: true, force: true });
console.log(`Prepared Node.js ${NODE_VERSION} and @deepseek-ai/dsh ${DSH_VERSION} for ${targetKey}.`);

import { createHash } from "node:crypto";
import { mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { spawn } from "node:child_process";
import path from "node:path";
import process from "node:process";
import { fileURLToPath } from "node:url";

const dshVersion = process.env.DSH_VERSION;
if (!dshVersion) {
  throw new Error("DSH_VERSION is required");
}

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const projectRoot = path.resolve(scriptDirectory, "..");
const targetKey = process.env.DSH_DESKTOP_RUNTIME_TARGET ?? `${process.platform}-${process.arch}`;
const outputRoot = process.env.DSH_RUNTIME_OUTPUT ?? path.join(projectRoot, "dist", "dsh-runtime");
const workRoot = path.join(outputRoot, ".pack", targetKey);
const archiveName = `dsh-${dshVersion}-${targetKey}.tar.gz`;
const archivePath = path.join(outputRoot, archiveName);

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

await rm(workRoot, { recursive: true, force: true });
await mkdir(workRoot, { recursive: true });
await mkdir(outputRoot, { recursive: true });

await writeFile(
  path.join(workRoot, "package.json"),
  `${JSON.stringify(
    {
      name: "deepseek-harness-desktop-runtime",
      version: "0.1.0",
      private: true,
      description: `Verified official DSH ${dshVersion} sidecar`,
      dependencies: {
        "@deepseek-ai/dsh": dshVersion,
      },
    },
    null,
    2,
  )}\n`,
);

const npmCli = process.env.npm_execpath;
if (!npmCli) {
  throw new Error("Run runtime packing through npm run pack:dsh-runtime");
}

await run(
  process.execPath,
  [
    npmCli,
    "install",
    "--omit=dev",
    "--no-audit",
    "--no-fund",
    "--registry=https://registry.npmjs.org",
  ],
  { cwd: workRoot },
);

const installed = JSON.parse(
  await readFile(path.join(workRoot, "node_modules", "@deepseek-ai", "dsh", "package.json"), "utf8"),
);
if (installed.version !== dshVersion) {
  throw new Error(`Expected DSH ${dshVersion}, installed ${installed.version}`);
}

const entry = path.join(workRoot, "node_modules", "@deepseek-ai", "dsh", "lib", "bin.js");
await readFile(entry);

await run("tar", ["-czf", archivePath, "-C", workRoot, "."]);

const bytes = await readFile(archivePath);
const sha256 = createHash("sha256").update(bytes).digest("hex");
await writeFile(path.join(outputRoot, `${archiveName}.sha256`), `${sha256}  ${archiveName}\n`);
await rm(path.join(outputRoot, ".pack"), { recursive: true, force: true });

console.log(`Packed ${archiveName} sha256=${sha256}`);

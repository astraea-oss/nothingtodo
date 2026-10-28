// Builds the portable Linux folder and tarball:
//   release/NothingToDo-<version>-linux-<arch>/ and release/NothingToDo-<version>-linux-<arch>.tar.gz
// Set NOTHINGTODO_SKIP_BUILD=1 to package an existing target/release/nothingtodo.
import { spawnSync } from "node:child_process";
import { chmod, copyFile, mkdir, readFile, rm } from "node:fs/promises";
import { existsSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

if (process.platform !== "linux") throw new Error("Build the Linux package on Linux.");

const appId = "uk.co.astraeaoss.nothingtodo";
const repository = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const { version } = JSON.parse(await readFile(join(repository, "package.json"), "utf8"));
const architecture = process.arch === "x64" ? "x86_64" : process.arch;
const outputRoot = join(repository, "release");
const folderName = `NothingToDo-${version}-linux-${architecture}`;
const staging = join(outputRoot, folderName);
const linux = join(repository, "packaging", "linux");
const icons = join(repository, "src-tauri", "icons");

function run(command, args) {
  const result = spawnSync(command, args, { cwd: repository, stdio: "inherit" });
  if (result.error) throw result.error;
  if (result.status !== 0) throw new Error(`${command} exited with status ${result.status}`);
}

async function copy(from, to, mode = 0o644) {
  await mkdir(dirname(to), { recursive: true });
  await copyFile(from, to);
  await chmod(to, mode);
}

if (process.env.NOTHINGTODO_SKIP_BUILD !== "1") {
  run("npm", ["run", "tauri", "--", "build", "--no-bundle", "--ci"]);
}

// Running the packaged app from release/ creates its data folder there. Never delete tasks.
if (existsSync(join(staging, "NothingToDo Data"))) {
  throw new Error(`${relative(repository, staging)} holds app data (NothingToDo Data). Move that folder out first, e.g. by copying the app folder elsewhere and using it from there.`);
}
await rm(staging, { recursive: true, force: true });
await mkdir(staging, { recursive: true });

await copy(join(repository, "target", "release", "nothingtodo"), join(staging, "nothingtodo"), 0o755);
await copy(join(linux, "NothingToDo-safe.sh"), join(staging, "NothingToDo-safe.sh"), 0o755);
await copy(join(linux, "install.sh"), join(staging, "install.sh"), 0o755);
await copy(join(linux, `${appId}.desktop`), join(staging, `${appId}.desktop`));
await copy(join(repository, "README.md"), join(staging, "README.md"));
await copy(join(repository, "LICENSE"), join(staging, "LICENSE"));
for (const [size, source] of [
  [32, "32x32.png"],
  [128, "128x128.png"],
  [256, "128x128@2x.png"],
  [512, "icon.png"],
]) {
  await copy(
    join(icons, source),
    join(staging, "share", "icons", "hicolor", `${size}x${size}`, "apps", `${appId}.png`),
  );
}

const archive = join(outputRoot, `${folderName}.tar.gz`);
await rm(archive, { force: true });
run("tar", ["-czf", archive, "-C", outputRoot, folderName]);
console.log(`Created ${relative(repository, archive)}`);

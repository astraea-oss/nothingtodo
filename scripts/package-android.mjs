// Builds an optimized, sideloadable arm64 APK: release/NothingToDo-<version>-android-arm64.apk
// Uses JAVA_HOME, ANDROID_HOME and NDK_HOME when set, otherwise the ~/Android layout
// (jdk17, Sdk, newest Sdk/ndk/*). It is signed with the key in
// src-tauri/gen/android/keystore.properties, or this machine's debug key when that is absent.
import { spawnSync } from "node:child_process";
import { copyFile, mkdir, readdir, readFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { homedir } from "node:os";
import { delimiter, dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const repository = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const { version } = JSON.parse(await readFile(join(repository, "package.json"), "utf8"));
const android = join(homedir(), "Android");
const env = { ...process.env };
env.JAVA_HOME ??= join(android, "jdk17");
env.ANDROID_HOME ??= join(android, "Sdk");
if (!env.NDK_HOME) {
  const ndks = existsSync(join(env.ANDROID_HOME, "ndk")) ? (await readdir(join(env.ANDROID_HOME, "ndk"))).sort() : [];
  if (ndks.length) env.NDK_HOME = join(env.ANDROID_HOME, "ndk", ndks.at(-1));
}
for (const name of ["JAVA_HOME", "ANDROID_HOME", "NDK_HOME"]) {
  if (!env[name] || !existsSync(env[name])) throw new Error(`${name} is not set and was not found under ${android}.`);
}
env.PATH = [join(env.JAVA_HOME, "bin"), join(env.ANDROID_HOME, "platform-tools"), env.PATH].join(delimiter);

const result = spawnSync("npm", ["run", "tauri", "--", "android", "build", "--apk", "--target", "aarch64", "--ci"], {
  cwd: repository, env, stdio: "inherit",
});
if (result.error) throw result.error;
if (result.status !== 0) throw new Error(`tauri android build exited with status ${result.status}`);

const output = join(repository, "release", `NothingToDo-${version}-android-arm64.apk`);
await mkdir(dirname(output), { recursive: true });
await copyFile(join(repository, "src-tauri", "gen", "android", "app", "build", "outputs", "apk", "universal", "release", "app-universal-release.apk"), output);
console.log(`Created ${relative(repository, output)}`);

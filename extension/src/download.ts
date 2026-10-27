import { execFile } from "node:child_process";
import * as fs from "node:fs";
import * as path from "node:path";
import { promisify } from "node:util";
import * as vscode from "vscode";

import { log } from "./state";

/** The Rust targets of the release archives, by `platform-arch` of Node. */
const TARGETS: Record<string, string> = {
  "win32-x64": "x86_64-pc-windows-msvc",
  "win32-arm64": "aarch64-pc-windows-msvc",
  "linux-x64": "x86_64-unknown-linux-gnu",
  "linux-arm64": "aarch64-unknown-linux-gnu",
  "darwin-x64": "x86_64-apple-darwin",
  "darwin-arm64": "aarch64-apple-darwin",
};

const EXE = process.platform === "win32" ? "toml_analyzer.exe" : "toml_analyzer";

/** The Rust target of this machine, if a release is built for it. */
export function hostTarget(): string | undefined {
  return TARGETS[`${process.platform}-${process.arch}`];
}

/** The GitHub repository of the extension: `https://github.com/owner/name`. */
function repository(context: vscode.ExtensionContext): string {
  const repo: unknown = context.extension.packageJSON?.repository;
  const url = typeof repo === "string" ? repo : (repo as { url?: string } | undefined)?.url;
  return (url ?? "").replace(/^git\+/, "").replace(/\.git$/, "");
}

/**
 * The server downloaded for the version of the extension, in the global
 * storage: downloaded from the GitHub release on first use.
 */
export async function downloadedServer(
  context: vscode.ExtensionContext,
  redownload = false,
): Promise<string | undefined> {
  const target = hostTarget();
  if (!target) {
    void vscode.window.showErrorMessage(
      `No TOML language server is released for ${process.platform}-${process.arch}: set \`toml.server.path\`.`,
    );
    return undefined;
  }
  const version: string = context.extension.packageJSON?.version ?? "latest";
  const root = path.join(context.globalStorageUri.fsPath, "server");
  const dir = path.join(root, version);
  const exe = path.join(dir, EXE);
  if (fs.existsSync(exe) && !redownload) {
    log(`Using the downloaded server ${exe}`);
    return exe;
  }

  const archive = `toml_analyzer-${target}.${process.platform === "win32" ? "zip" : "tar.gz"}`;
  const releases = `${repository(context)}/releases`;
  // The release of this version, else the latest one.
  const urls = [`${releases}/download/${version}/${archive}`, `${releases}/latest/download/${archive}`];
  try {
    await vscode.window.withProgress(
      { location: vscode.ProgressLocation.Notification, title: "Downloading the TOML language server" },
      () => download(urls, root, dir, archive),
    );
    log(`Downloaded the server to ${exe}`);
  } catch (error) {
    log(`Cannot download the TOML language server: ${String(error)}`);
    void vscode.window.showErrorMessage(
      `Cannot download the TOML language server from ${releases}: ${String(error)}. Set \`toml.server.path\` to use a local one.`,
    );
    return undefined;
  }
  // Older versions are not used anymore.
  for (const old of fs.readdirSync(root)) {
    if (old !== version) {
      fs.rmSync(path.join(root, old), { recursive: true, force: true });
    }
  }
  return exe;
}

/** Fetches the first of `urls` that exists and extracts it into `dir`. */
async function download(urls: string[], root: string, dir: string, archive: string): Promise<void> {
  let response: Response | undefined;
  for (const url of urls) {
    log(`Downloading ${url}`);
    response = await fetch(url);
    if (response.ok) {
      break;
    }
    log(`${url}: ${response.status} ${response.statusText}`);
  }
  if (!response?.ok) {
    throw new Error(`no release archive ${archive}`);
  }

  // Extracted aside, then moved: another window may be downloading too.
  fs.mkdirSync(root, { recursive: true });
  const tmp = fs.mkdtempSync(path.join(root, ".download-"));
  try {
    const file = path.join(tmp, archive);
    fs.writeFileSync(file, Buffer.from(await response.arrayBuffer()));
    await promisify(execFile)(tarCommand(), ["-xf", file, "-C", tmp]);
    const exe = path.join(tmp, EXE);
    if (!fs.existsSync(exe)) {
      throw new Error(`${archive} has no ${EXE}`);
    }
    if (process.platform !== "win32") {
      fs.chmodSync(exe, 0o755);
    }
    fs.rmSync(file);
    fs.rmSync(dir, { recursive: true, force: true });
    fs.renameSync(tmp, dir);
  } finally {
    fs.rmSync(tmp, { recursive: true, force: true });
  }
}

/** `tar`; on Windows the bsdtar of the system, which also extracts zip files. */
function tarCommand(): string {
  if (process.platform === "win32") {
    return path.join(process.env.SystemRoot ?? "C:\\Windows", "System32", "tar.exe");
  }
  return "tar";
}

// CI helper for stamping the release version and writing Tauri's standard `latest.json` manifest
// for both Windows (`windows-x86_64`) and macOS (`darwin-aarch64`).
//
//   node scripts/ci-release-meta.mjs bump --run-number 12 [--version 0.2.0]
//   node scripts/ci-release-meta.mjs manifest --version 0.1.12 --repo Kamo2323/wren \
//     --installer Wren_0.1.12_x64-setup.exe --sig-file path/to/win.sig \
//     [--mac-installer Wren_0.1.12_aarch64.app.tar.gz --mac-sig-file path/to/mac.sig] \
//     --notes-file path/to/notes.md --out path/to/latest.json
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const SEMVER_RE = /^(\d+)\.(\d+)\.(\d+)(?:-[0-9A-Za-z.-]+)?$/;

export function computeVersion(baseVersion, { runNumber, explicitVersion }) {
  if (explicitVersion && explicitVersion.trim() !== "") {
    const clean = explicitVersion.trim().replace(/^v/i, "");
    if (!SEMVER_RE.test(clean)) throw new Error(`Invalid explicit version: ${explicitVersion}`);
    return clean;
  }
  const m = SEMVER_RE.exec(String(baseVersion).trim());
  if (!m) throw new Error(`Invalid base version in tauri.conf.json: ${baseVersion}`);
  const patch = Number(runNumber);
  if (!Number.isInteger(patch) || patch < 1) throw new Error(`Invalid run number: ${runNumber}`);
  return `${m[1]}.${m[2]}.${patch}`;
}

export function stampCargoVersion(cargoToml, version) {
  let replaced = false;
  const lines = cargoToml.split(/\r?\n/);
  const eol = cargoToml.includes("\r\n") ? "\r\n" : "\n";
  let inPackage = false;
  const out = lines.map((line) => {
    const trimmed = line.trim();
    if (trimmed.startsWith("[")) inPackage = trimmed === "[package]";
    if (inPackage && !replaced && /^version\s*=\s*"[^"]*"/.test(trimmed)) {
      replaced = true;
      return line.replace(/^(\s*version\s*=\s*)"[^"]*"/, `$1"${version}"`);
    }
    return line;
  });
  if (!replaced) throw new Error("Could not find version under [package] in Cargo.toml");
  return out.join(eol);
}

export function buildManifest({
  version,
  repo,
  installer,
  signature,
  macInstaller,
  macSignature,
  macPlatform = "darwin-aarch64",
  notes,
  pubDate,
}) {
  const cleanVer = version.trim().replace(/^v/i, "");
  const platforms = {};

  if (installer && signature) {
    const cleanSig = signature.trim();
    if (!cleanSig) throw new Error("Signature is empty");
    platforms["windows-x86_64"] = {
      signature: cleanSig,
      url: `https://github.com/${repo}/releases/download/v${cleanVer}/${encodeURIComponent(installer)}`,
    };
  }
  if (macInstaller && macSignature) {
    const cleanMacSig = macSignature.trim();
    if (!cleanMacSig) throw new Error("macOS signature is empty");
    platforms[macPlatform] = {
      signature: cleanMacSig,
      url: `https://github.com/${repo}/releases/download/v${cleanVer}/${encodeURIComponent(macInstaller)}`,
    };
  }
  if (Object.keys(platforms).length === 0) {
    throw new Error("At least one platform installer and signature is required");
  }

  return {
    version: cleanVer,
    notes: (notes ?? "").trim(),
    pub_date: pubDate ?? new Date().toISOString(),
    platforms,
  };
}

function parseArgs(argv) {
  const out = { _: [] };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a.startsWith("--")) {
      out[a.slice(2)] = argv[++i] ?? "";
    } else {
      out._.push(a);
    }
  }
  return out;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = parseArgs(process.argv.slice(2));
  const cmd = args._[0];
  const root = join(dirname(fileURLToPath(import.meta.url)), "..");

  if (cmd === "bump") {
    const tauriPath = join(root, "src-tauri", "tauri.conf.json");
    const pkgPath = join(root, "package.json");
    const cargoPath = join(root, "src-tauri", "Cargo.toml");

    const tauriConf = JSON.parse(readFileSync(tauriPath, "utf8"));
    const nextVersion = computeVersion(tauriConf.version, {
      runNumber: args["run-number"],
      explicitVersion: args.version,
    });

    tauriConf.version = nextVersion;
    writeFileSync(tauriPath, `${JSON.stringify(tauriConf, null, 2)}\n`);

    const pkg = JSON.parse(readFileSync(pkgPath, "utf8"));
    pkg.version = nextVersion;
    writeFileSync(pkgPath, `${JSON.stringify(pkg, null, 2)}\n`);

    const cargo = readFileSync(cargoPath, "utf8");
    writeFileSync(cargoPath, stampCargoVersion(cargo, nextVersion));

    process.stdout.write(`${nextVersion}\n`);
  } else if (cmd === "manifest") {
    const signature = args["sig-file"] ? readFileSync(args["sig-file"], "utf8") : "";
    const macSignature = args["mac-sig-file"] ? readFileSync(args["mac-sig-file"], "utf8") : "";
    const notes = args["notes-file"] ? readFileSync(args["notes-file"], "utf8") : "";
    const manifest = buildManifest({
      version: args.version,
      repo: args.repo,
      installer: args.installer,
      signature,
      macInstaller: args["mac-installer"],
      macSignature,
      macPlatform: args["mac-platform"] || "darwin-aarch64",
      notes,
    });
    writeFileSync(args.out, `${JSON.stringify(manifest, null, 2)}\n`);
  } else {
    console.error("Usage: node scripts/ci-release-meta.mjs <bump|manifest> [options]");
    process.exit(1);
  }
}

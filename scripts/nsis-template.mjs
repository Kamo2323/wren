// Makes src-tauri/installer/installer.nsi: the NSIS template that the installed Tauri CLI bundles
// (extracted from its binary, so it always matches that CLI version) with Wren's two changes:
//
//   1. The per-user install folder is %LOCALAPPDATA%\Programs\Wren, not Tauri's default
//      %LOCALAPPDATA%\Wren — that folder is Wren's data root (database, audio, speech runtime,
//      models, logs), and the program must never live in it.
//   2. The installer refuses to install into %LOCALAPPDATA%\Wren even if asked (/D=, the folder
//      page, or a previous install's registry entry).
//
//   node scripts/nsis-template.mjs           write the template
//   node scripts/nsis-template.mjs --check   exit 1 if the committed template is out of date
//                                            (scripts/release.ps1 runs this before every build)
import { readFileSync, writeFileSync, readdirSync, existsSync } from "node:fs";
import { join, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const repo = join(dirname(fileURLToPath(import.meta.url)), "..");
const out = join(repo, "src-tauri", "installer", "installer.nsi");

function cliBinary() {
  const dir = join(repo, "node_modules", "@tauri-apps", "cli-win32-x64-msvc");
  if (!existsSync(dir)) throw new Error(`Tauri CLI not found in ${dir} (run npm install)`);
  const node = readdirSync(dir).find((f) => f.endsWith(".node"));
  if (!node) throw new Error(`no .node binary in ${dir}`);
  return join(dir, node);
}

function extract() {
  const bin = readFileSync(cliBinary());
  const start = bin.indexOf(Buffer.from("Unicode true\r\nManifestDPIAware true\r\n"));
  if (start < 0) throw new Error("the NSIS template was not found in the Tauri CLI binary");
  const end = bin.indexOf(0, start);
  const text = bin.subarray(start, end).toString("utf8");
  if (!text.trimEnd().endsWith("FunctionEnd") || !text.includes("Section Uninstall")) {
    throw new Error("the NSIS template in the Tauri CLI binary has an unexpected shape");
  }
  return text;
}

function patch(text) {
  const swaps = [
    [
      '    !else if "${INSTALLMODE}" == "currentUser"\r\n      StrCpy $INSTDIR "$LOCALAPPDATA\\${PRODUCTNAME}"\r\n    !endif\r\n\r\n    Call RestorePreviousInstallLocation\r\n',
      '    !else if "${INSTALLMODE}" == "currentUser"\r\n' +
        "      ; Wren: %LOCALAPPDATA%\\Wren is Wren's data root (database, audio, speech runtime, models,\r\n" +
        "      ; logs), so the program goes to %LOCALAPPDATA%\\Programs\\Wren instead.\r\n" +
        '      StrCpy $INSTDIR "$LOCALAPPDATA\\Programs\\${PRODUCTNAME}"\r\n    !endif\r\n\r\n    Call RestorePreviousInstallLocation\r\n' +
        '    ${If} $INSTDIR == "$LOCALAPPDATA\\${PRODUCTNAME}"\r\n' +
        '      StrCpy $INSTDIR "$LOCALAPPDATA\\Programs\\${PRODUCTNAME}"\r\n' +
        "    ${EndIf}\r\n",
    ],
    [
      "Section Install\r\n  SetOutPath $INSTDIR\r\n",
      "Section Install\r\n" +
        "  ; Wren: never install into the data root, however the folder was chosen.\r\n" +
        '  ${If} $INSTDIR == "$LOCALAPPDATA\\${PRODUCTNAME}"\r\n' +
        '  ${OrIf} $INSTDIR == "$LOCALAPPDATA\\${PRODUCTNAME}\\"\r\n' +
        '    MessageBox MB_ICONSTOP "Wren keeps your history, speech engine and models in $INSTDIR, so it cannot be installed there. Please choose another folder." /SD IDOK\r\n' +
        "    Abort\r\n" +
        "  ${EndIf}\r\n" +
        "  SetOutPath $INSTDIR\r\n",
    ],
  ];
  for (const [from, to] of swaps) {
    const n = text.split(from).length - 1;
    if (n !== 1) throw new Error(`patch anchor found ${n} times (expected once): ${JSON.stringify(from.slice(0, 60))}`);
    text = text.replace(from, () => to);
  }
  return text;
}

const want = patch(extract());
if (process.argv.includes("--check")) {
  const have = existsSync(out) ? readFileSync(out, "utf8") : "";
  if (have !== want) {
    console.error(`${out} does not match the Tauri CLI's template; run: node scripts/nsis-template.mjs`);
    process.exit(1);
  }
  console.log("NSIS template up to date");
} else {
  writeFileSync(out, want);
  console.log(`wrote ${out}`);
}

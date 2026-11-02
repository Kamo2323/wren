<#
.SYNOPSIS
  Generates Wren's speech fixtures with the en-GB "Microsoft Hazel Desktop" SAPI voice, 16 kHz mono 16-bit.

.DESCRIPTION
  Writes src-tauri\tests\fixtures\<name>.wav for each sentence below, a 3 s silent WAV, and
  manifest.json (file, sentence, words the engine must hear). Also writes the 10 s warm-up clip the
  sidecar transcribes before it reports ready: sidecar\assets\warmup.wav.
  Re-running overwrites the files; the output is deterministic for a given voice install.
#>
param(
    [string]$RepoRoot = (Join-Path $PSScriptRoot '..')
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Speech

$RepoRoot = (Resolve-Path $RepoRoot).Path
$fixtures = Join-Path $RepoRoot 'src-tauri\tests\fixtures'
$assets = Join-Path $RepoRoot 'sidecar\assets'
New-Item -ItemType Directory -Force $fixtures, $assets | Out-Null

# name, sentence, words that must appear in the transcript (case-insensitive).
$sentences = @(
    @{ Name = 'weather';  Text = 'The weather in London is grey and drizzly again this morning.'; Expect = @('weather', 'London', 'morning') },
    @{ Name = 'meeting';  Text = 'Please move the meeting with the design team to half past three on Thursday.'; Expect = @('meeting', 'design', 'Thursday') },
    # A made-up name: the model will not know it, so only the words around it are required.
    @{ Name = 'madeup';   Text = 'Send the quarterly report to Zorvath Quillby before the end of the week.'; Expect = @('quarterly', 'report', 'week') },
    @{ Name = 'cafe';     Text = 'I would like two coffees and a croissant to take away, please.'; Expect = @('coffees', 'croissant', 'please') },
    @{ Name = 'long';     Text = 'Wren is a small dictation app that runs on this computer. Hold the two keys, say what you want, and let go; the words appear in the window you were using.'; Expect = @('dictation', 'computer', 'window') }
)
$warmupText = 'This is the warm-up clip for Wren. It is read once when the speech engine starts, so that the first real dictation is as quick as all the others.'

$format = New-Object System.Speech.AudioFormat.SpeechAudioFormatInfo(16000, [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen, [System.Speech.AudioFormat.AudioChannel]::Mono)

function Get-WavSeconds([string]$path) {
    $bytes = (Get-Item $path).Length - 44
    return [math]::Round($bytes / 32000.0, 2)
}

function Save-Speech([string]$text, [string]$path) {
    $synth = New-Object System.Speech.Synthesis.SpeechSynthesizer
    try {
        $synth.SelectVoice('Microsoft Hazel Desktop')
        $synth.Rate = 0
        $synth.SetOutputToWaveFile($path, $format)
        $synth.Speak($text)
    } finally {
        $synth.SetOutputToNull()
        $synth.Dispose()
    }
}

function Save-Silence([string]$path, [int]$seconds) {
    $dataLen = 16000 * 2 * $seconds
    $fs = [System.IO.File]::Create($path)
    $w = New-Object System.IO.BinaryWriter($fs)
    try {
        $w.Write([System.Text.Encoding]::ASCII.GetBytes('RIFF')); $w.Write([int](36 + $dataLen))
        $w.Write([System.Text.Encoding]::ASCII.GetBytes('WAVEfmt ')); $w.Write([int]16)
        $w.Write([int16]1); $w.Write([int16]1); $w.Write([int]16000); $w.Write([int]32000)
        $w.Write([int16]2); $w.Write([int16]16)
        $w.Write([System.Text.Encoding]::ASCII.GetBytes('data')); $w.Write([int]$dataLen)
        $w.Write((New-Object byte[] $dataLen))
    } finally { $w.Dispose() }
}

$manifest = @()
foreach ($s in $sentences) {
    $path = Join-Path $fixtures "$($s.Name).wav"
    Save-Speech $s.Text $path
    $secs = Get-WavSeconds $path
    Write-Host ("{0,-10} {1,5} s  {2}" -f $s.Name, $secs, $s.Text)
    $manifest += [ordered]@{ file = "$($s.Name).wav"; text = $s.Text; expect = $s.Expect; seconds = $secs }
}
$silent = Join-Path $fixtures 'silent.wav'
Save-Silence $silent 3
Write-Host ("{0,-10} {1,5} s  (digital silence)" -f 'silent', (Get-WavSeconds $silent))
$manifest += [ordered]@{ file = 'silent.wav'; text = ''; expect = @(); seconds = 3 }

$json = ConvertTo-Json -InputObject $manifest -Depth 4
[System.IO.File]::WriteAllText((Join-Path $fixtures 'manifest.json'), $json, (New-Object System.Text.UTF8Encoding($false)))

$warm = Join-Path $assets 'warmup.wav'
Save-Speech $warmupText $warm
Write-Host ("{0,-10} {1,5} s  {2}" -f 'warmup', (Get-WavSeconds $warm), $warm)

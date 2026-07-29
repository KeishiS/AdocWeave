param(
  [Parameter(Mandatory = $true)]
  [string]$DestinationDirectory,
  [string]$ConfigPath = "release/windows-dist-bootstrap.json",
  [ValidateRange(1, 300)]
  [int]$DownloadTimeoutSeconds = 60,
  [ValidateRange(1, 300)]
  [int]$ExtractionTimeoutSeconds = 30
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$config = Get-Content -LiteralPath $ConfigPath -Raw | ConvertFrom-Json
$temporaryDirectory = Join-Path ([IO.Path]::GetTempPath()) "adocweave-cargo-dist-$([Guid]::NewGuid())"
$archivePath = Join-Path $temporaryDirectory $config.asset
$stagedExecutable = Join-Path $temporaryDirectory $config.executable
$destinationExecutable = Join-Path $DestinationDirectory $config.executable

try {
  New-Item -ItemType Directory -Path $temporaryDirectory | Out-Null
  Invoke-WebRequest `
    -Uri $config.url `
    -OutFile $archivePath `
    -MaximumRedirection 5 `
    -TimeoutSec $DownloadTimeoutSeconds

  $actualHash = (Get-FileHash -LiteralPath $archivePath -Algorithm SHA256).Hash.ToLowerInvariant()
  if ($actualHash -cne $config.sha256) {
    throw "cargo-dist archive checksum mismatch"
  }

  Add-Type -AssemblyName System.IO.Compression.FileSystem
  $archive = [IO.Compression.ZipFile]::OpenRead($archivePath)
  try {
    $actualEntries = @($archive.Entries | ForEach-Object { $_.FullName } | Sort-Object)
    $expectedEntries = @($config.archiveEntries | Sort-Object)
    if (Compare-Object -CaseSensitive $actualEntries $expectedEntries) {
      throw "cargo-dist archive contains an unexpected entry"
    }
    foreach ($entryName in $actualEntries) {
      if ([IO.Path]::GetFileName($entryName) -cne $entryName) {
        throw "cargo-dist archive contains a path entry"
      }
    }

    $entry = $archive.GetEntry($config.executable)
    if ($null -eq $entry) {
      throw "cargo-dist archive does not contain the expected executable"
    }
    $input = $entry.Open()
    $output = [IO.File]::Open(
      $stagedExecutable,
      [IO.FileMode]::CreateNew,
      [IO.FileAccess]::Write,
      [IO.FileShare]::None
    )
    try {
      $cancellation = [Threading.CancellationTokenSource]::new(
        [TimeSpan]::FromSeconds($ExtractionTimeoutSeconds)
      )
      try {
        $input.CopyToAsync($output, 81920, $cancellation.Token).GetAwaiter().GetResult()
      } finally {
        $cancellation.Dispose()
      }
    } finally {
      $output.Dispose()
      $input.Dispose()
    }
  } finally {
    $archive.Dispose()
  }

  New-Item -ItemType Directory -Path $DestinationDirectory -Force | Out-Null
  Move-Item -LiteralPath $stagedExecutable -Destination $destinationExecutable -Force
} finally {
  Remove-Item -LiteralPath $temporaryDirectory -Recurse -Force -ErrorAction SilentlyContinue
}

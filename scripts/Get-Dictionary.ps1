# Downloads Princeton's WordNet 3.1 dictionary, used for the definitions in
# Muelle's search window (English only). Usage: scripts/Get-Dictionary.ps1
# The licence text is at the top of each installed data file.
param([string]$Destination)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12

$url = 'https://wordnetcode.princeton.edu/wn3.1.dict.tar.gz'
$sha256 = '3F7D8BE8EF6ECC7167D39B10D66954EC734280B5BDCD57F7D9EAFE429D11C22A'
if (-not $Destination) {
    $Destination = Join-Path $env:LOCALAPPDATA 'Muelle\dictionary'
}
$work = Join-Path ([IO.Path]::GetTempPath()) "muelle-dictionary-$([Guid]::NewGuid())"

New-Item -ItemType Directory -Force $work, "$work\files", $destination | Out-Null
try {
    $archive = Join-Path $work 'dictionary.tar.gz'
    Write-Host 'Downloading the dictionary (about 16 MB)...'
    Invoke-WebRequest $url -OutFile $archive -UseBasicParsing
    if ((Get-FileHash $archive -Algorithm SHA256).Hash -ne $sha256) {
        throw 'The download does not match the expected checksum.'
    }
    & "$env:SystemRoot\System32\tar.exe" -xzf $archive -C "$work\files"
    if ($LASTEXITCODE -ne 0) { throw 'Could not unpack the dictionary.' }

    foreach ($part in 'noun', 'verb', 'adj', 'adv') {
        foreach ($name in "index.$part", "data.$part", "$part.exc") {
            Copy-Item "$work\files\dict\$name" $destination -Force
        }
    }
    Write-Host "Dictionary installed in $destination"
}
finally {
    Remove-Item $work -Recurse -Force -ErrorAction SilentlyContinue
}

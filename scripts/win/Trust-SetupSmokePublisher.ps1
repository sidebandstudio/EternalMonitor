# Test-only publisher approval for the disposable Windows installer runner.
param(
    [Parameter(Mandatory)][string]$VendorSetup,
    [Parameter(Mandatory)][string]$LogDirectory
)
$ErrorActionPreference = 'Stop'
$logDir = (New-Item -ItemType Directory -Path $LogDirectory -Force).FullName
$log = Join-Path $logDir 'publisher-trust.log'
Start-Transcript -LiteralPath $log | Out-Null
try {
    if ((Get-FileHash -LiteralPath $VendorSetup -Algorithm SHA256).Hash -ne
        'ca10b85babecfb636c85b3f04d2306968d4f940dd3dd35767f866207bfba846e') {
        throw 'The bundled installer does not match the package reviewed for this publisher approval.'
    }
    $scratch = Join-Path $env:RUNNER_TEMP 'setup-smoke-publisher'
    New-Item -ItemType Directory -Path $scratch -Force | Out-Null
    $archive = Join-Path $scratch 'driver.zip'
    $catalog = Join-Path $scratch 'mttvdd.cat'
    # The pinned 25.5.2 x64 archive and bundled vendor installer contain
    # different catalogs signed by the same exact SignPath Foundation cert.
    $url = 'https://github.com/VirtualDrivers/Virtual-Display-Driver/releases/download/25.5.2/Signed-Driver-v24.12.24-x64.zip'
    Invoke-WebRequest -Uri $url -OutFile $archive -TimeoutSec 30 -MaximumRetryCount 2 -RetryIntervalSec 3
    $zip = [IO.Compression.ZipFile]::OpenRead($archive)
    try {
        [IO.Compression.ZipFileExtensions]::ExtractToFile($zip.GetEntry('mttvdd.cat'), $catalog, $true)
    } finally {
        $zip.Dispose()
    }
    if ((Get-FileHash $catalog -Algorithm SHA256).Hash -ne '08a0093fc9b2e32b287a6f8a77ca4de0a31830d29fc33d2b13a918dc859468f6') {
        throw 'The downloaded driver catalog hash does not match the reviewed catalog.'
    }

    $cms = [Security.Cryptography.Pkcs.SignedCms]::new()
    $cms.Decode([IO.File]::ReadAllBytes($catalog))
    $cms.CheckSignature($true)
    $signer = $cms.SignerInfos[0].Certificate
    $sha256 = [Security.Cryptography.HashAlgorithmName]::SHA256
    $signerHash = $signer.GetCertHashString($sha256)
    if ($signerHash -ne '69DFD3E1CBD49F039463EDFE4C0D876C1DC7FDCD3AEF08700A41355DC6C15FDB') {
        throw 'The catalog signer does not match the signer extracted from the bundled driver.'
    }
    Write-Host "Pinned catalog signer $($signer.Subject). SHA-256 $signerHash."
    Write-Host "Catalog Authenticode before approval: $((Get-AuthenticodeSignature -LiteralPath $catalog).Status)."

    $chain = [Security.Cryptography.X509Certificates.X509Chain]::new($true)
    try {
        # Inspect the path to the root. Authenticode is checked separately
        # and this does not change Windows' revocation or driver policies.
        $chain.ChainPolicy.RevocationMode = 'NoCheck'
        $chain.ChainPolicy.ExtraStore.AddRange($cms.Certificates)
        if (!$chain.Build($signer)) {
            $statuses = @($chain.ChainStatus | ForEach-Object { $_.Status.ToString() })
            if ($statuses.Count -ne 1 -or $statuses[0] -ne 'UntrustedRoot') {
                throw "The catalog chain did not validate. $($statuses -join ', ')."
            }
            $root = $chain.ChainElements[$chain.ChainElements.Count - 1].Certificate
            if ($root.GetCertHashString($sha256) -ne '7B9D553E1C92CB6E8803E137F4F287D4363757F5D44B37D52F9FCA22FB97DF86') {
                throw 'The untrusted root does not match the reviewed GlobalSign root.'
            }
            $store = [Security.Cryptography.X509Certificates.X509Store]::new('Root', 'LocalMachine')
            try {
                $store.Open('ReadWrite')
                $store.Add($root)
                Write-Host "Added the pinned GlobalSign root because the machine chain reported UntrustedRoot."
            } finally { $store.Dispose() }
            if (!$chain.Build($signer)) { throw 'The catalog chain still fails after adding the pinned root.' }
        } else {
            Write-Host 'The machine already validates the chain. No root certificate was added.'
        }
    } finally { $chain.Dispose() }

    $store = [Security.Cryptography.X509Certificates.X509Store]::new('TrustedPublisher', 'LocalMachine')
    try {
        $store.Open('ReadWrite')
        $store.Add($signer)
        Write-Host "Approved the pinned SignPath Foundation leaf in LocalMachine TrustedPublisher."
    } finally { $store.Dispose() }
    $authenticode = (Get-AuthenticodeSignature -LiteralPath $catalog).Status
    Write-Host "Catalog Authenticode after approval: $authenticode."
    if ($env:GITHUB_STEP_SUMMARY) {
        "Setup smoke approved the pinned SignPath Foundation driver publisher on the disposable runner. Catalog Authenticode reported $authenticode after approval." |
            Add-Content -LiteralPath $env:GITHUB_STEP_SUMMARY
    }
} finally {
    Stop-Transcript | Out-Null
}

# Static checks for how Setup hands its language to the app: an interactive run writes the
# app's code for the chosen language to {app}\language.txt, a silent run (an in-app update)
# keeps the earlier file, and the uninstaller removes it. Nothing is compiled or installed.
# The checks also run against broken copies of the script, which they must reject.
param([string]$IssPath)
$ErrorActionPreference = 'Stop'
$source = if ($IssPath) { $IssPath } else { Join-Path $PSScriptRoot '..\EternalMonitor.iss' }
$iss = Get-Content -Raw -Encoding UTF8 $source
# The host's language codes: one string file per language (see host/src/i18n/mod.rs).
$strings = Join-Path $PSScriptRoot '..\..\host\src\i18n\strings'
$codes = @(Get-ChildItem $strings -Filter '*.json' | ForEach-Object { $_.BaseName })
if ($codes.Count -ne 11) { throw "Expected the host's 11 languages in $strings, found $($codes.Count)" }

# The lines of one [Section], without those an #ifdef leaves out of some builds.
function Get-Section([string]$Text, [string]$Name) {
    $lines = @(); $inside = $false; $depth = 0
    foreach ($line in $Text -split "\r?\n") {
        if ($line -match '^\[(\w+)\]') { $inside = $Matches[1] -eq $Name; continue }
        if (!$inside) { continue }
        if ($line -match '^#if') { $depth++ } elseif ($line -match '^#endif') { $depth-- }
        elseif ($depth -eq 0) { $lines += $line }
    }
    $lines
}

function Get-Failures([string]$Text) {
    $failures = @()
    $code = [regex]::Match($Text, '(?s)procedure CurStepChanged\(CurStep: TSetupStep\);.*?\nend;').Value
    $write = [regex]::Match($code,
        "(?s)if (?<cond>[^;]*?) then\s+if not SaveStringToFile\(\s*ExpandConstant\('\{app\}\\language\.txt'\),\s*ExpandConstant\('(?<value>[^']*)'\)")
    if (!$write.Success) {
        $failures += 'CurStepChanged does not write {app}\language.txt'
    } else {
        if ($write.Groups['cond'].Value -notmatch '\bnot WizardSilent\b') { $failures += 'A silent run overwrites language.txt' }
        if ($write.Groups['cond'].Value -notmatch '\bssPostInstall\b') { $failures += 'language.txt is not written after the files are installed' }
        if ($write.Groups['value'].Value -ne '{cm:AppLanguageCode}') { $failures += "language.txt holds $($write.Groups['value'].Value), not {cm:AppLanguageCode}" }
    }
    if (!(Get-Section $Text 'UninstallDelete' | Where-Object { $_ -match '^Type: files; Name: "\{app\}\\language\.txt"' })) {
        $failures += 'The uninstaller does not remove language.txt in every build'
    }

    $languages = @(Get-Section $Text 'Languages' | Where-Object { $_ -match '^Name: "(\w+)";' } | ForEach-Object { ($_ -split '"')[1] })
    if ($languages.Count -ne 11) { $failures += "Setup has $($languages.Count) languages, not 11" }
    $messages = Get-Section $Text 'CustomMessages'
    $used = @()
    foreach ($language in $languages) {
        $values = @($messages | Where-Object { $_ -match "^$language\.AppLanguageCode=" } | ForEach-Object { ($_ -split '=', 2)[1].Trim() })
        if ($values.Count -ne 1) { $failures += "$language defines AppLanguageCode $($values.Count) times"; continue }
        if ($codes -cnotcontains $values[0]) { $failures += "$language's AppLanguageCode $($values[0]) is not one of the host's codes" }
        $used += $values[0]
    }
    if (@($used | Select-Object -Unique).Count -ne $used.Count) { $failures += 'Two languages share an AppLanguageCode' }
    $failures
}

$failures = @(Get-Failures $iss)
if ($failures) { throw "Language checks failed:`n  " + ($failures -join "`n  ") }

# Each broken copy must fail.
$broken = [ordered]@{
    'silent run writes' = $iss.Replace(' and not WizardSilent then', ' then')
    'fixed code' = $iss.Replace("ExpandConstant('{cm:AppLanguageCode}')", "ExpandConstant('en')")
    'not removed' = $iss.Replace('Type: files; Name: "{app}\language.txt"', '')
    'removed only with the driver' = $iss.Replace("[UninstallDelete]`n", "[UninstallDelete]`n#ifdef IncludeDriver`n").Replace("[UninstallDelete]`r`n", "[UninstallDelete]`r`n#ifdef IncludeDriver`r`n")
    'unknown code' = $iss.Replace('german.AppLanguageCode=de', 'german.AppLanguageCode=de-DE')
    'missing code' = $iss.Replace('korean.AppLanguageCode=ko', '')
    'case differs' = $iss.Replace('chinesesimplified.AppLanguageCode=zh-Hans', 'chinesesimplified.AppLanguageCode=zh-hans')
}
foreach ($case in $broken.Keys) {
    if ($broken[$case] -ceq $iss) { throw "The '$case' copy is the real script; update this test" }
    if (!@(Get-Failures $broken[$case])) { throw "The checks pass a broken script: $case" }
}
Write-Output "PASS: guarded language.txt write, uninstall removal, 11 language codes ($($codes -join ', ')), $($broken.Count) broken copies rejected"

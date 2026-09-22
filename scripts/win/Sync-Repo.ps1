param([Parameter(Mandatory=$true)][string]$Branch)
$ErrorActionPreference = 'Stop'
Set-Location 'D:\AgentWork\Eternal-Monitor'
git check-ref-format --branch $Branch
if ($LASTEXITCODE -ne 0) { throw 'Invalid branch name' }
$dirty = git status --porcelain
if ($LASTEXITCODE -ne 0 -or $dirty) { throw 'Windows checkout is not clean; refusing to replace local work' }
git fetch origin --prune
if ($LASTEXITCODE -ne 0) { throw 'git fetch failed' }
git show-ref --verify "refs/remotes/origin/$Branch"
if ($LASTEXITCODE -ne 0) { throw 'Remote branch does not exist' }
git checkout -B $Branch "origin/$Branch"
if ($LASTEXITCODE -ne 0) { throw 'git checkout failed' }
git reset --hard "origin/$Branch"
if ($LASTEXITCODE -ne 0) { throw 'git reset failed' }
git rev-parse HEAD

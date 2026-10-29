# Windows code signing

The Release workflow signs the Windows installer with
[Azure Artifact Signing](https://learn.microsoft.com/en-us/azure/artifact-signing/)
once the `release` environment has the settings below. Until then it builds an
unsigned installer, as it always has, and puts a warning on the run.

## What gets signed

Inno Setup signs four files while it compiles: `EternalMonitor-Setup.exe`, the
uninstaller it writes, `EternalMonitor-host.exe` and `EternalMonitor-updater.exe`.
`SignTool=eternal` in `installer/EternalMonitor.iss` turns this on, and
`scripts/build-installer.ps1 -SignCommand` supplies the command.

The FFmpeg DLLs, `ffmpeg.exe` and the Virtual Display Driver setup ship as their
publishers built them. We didn't build them, so they don't carry our signature.

Every signature gets a timestamp from Microsoft's timestamp server. Artifact
Signing certificates expire after three days, and the timestamp is what keeps a
signature valid after that. The workflow's "Check the Authenticode signatures"
step fails the run if Setup or either exe isn't validly signed and timestamped.

Signing doesn't remove the SmartScreen "Windows protected your PC" prompt on
day one. SmartScreen builds reputation for the publisher name as people download
signed releases, and that reputation carries from one release to the next.

## One-time Azure setup

The person whose identity goes on the certificate does this in the Azure portal.
Identity validation can't be done from the CLI.

1. Use a pay-as-you-go Azure subscription. For an Individual certificate, the
   subscription's billing account must have the account type Individual, and its
   legal name and sold-to address must match your government ID. Individual
   certificates are only available in the United States and Canada.
2. Under Subscriptions, Resource providers, register `Microsoft.CodeSigning`.
3. Search for Artifact Signing Accounts and create one on the Basic tier. Note the
   region's endpoint from
   [Microsoft's region table](https://learn.microsoft.com/en-us/azure/artifact-signing/quickstart#azure-regions-that-support-artifact-signing).
   East US is `https://eus.codesigning.azure.net`.
4. On the account's Access control (IAM), give yourself the Artifact Signing
   Identity Verifier role. Owner alone can't create identity validations.
5. Under Identity validations, choose Individual, New identity, Public. When the
   status changes to Action Required, open the link and finish the ID check with
   AU10TIX and Microsoft Authenticator. Microsoft quotes 1 to 20 business days.
   An Organization validation instead puts a registered business name on the
   certificate. It needs the business's legal details and an email address on
   its domain.
6. Under Certificate profiles, create a Public Trust profile that uses the
   completed identity validation.
7. In Microsoft Entra ID, register an app for the workflow, for example
   `eternalmonitor-release-signing`. Under Certificates & secrets, Federated
   credentials, add a GitHub Actions credential for organization
   `sidebandstudio`, repository `EternalMonitor`, entity type Environment,
   environment `release`. The app needs no client secret.
8. On the Artifact Signing account's Access control (IAM), assign the Artifact
   Signing Certificate Profile Signer role to that app.

## GitHub settings

The first Release run that includes this workflow creates the `release`
environment. Create it under Settings, Environments if it isn't there yet, then add:

| Secret | Value |
| --- | --- |
| `AZURE_CLIENT_ID` | The app's Application (client) ID |
| `AZURE_TENANT_ID` | The Directory (tenant) ID |
| `AZURE_SUBSCRIPTION_ID` | The subscription that holds the signing account |

| Variable | Value |
| --- | --- |
| `ARTIFACT_SIGNING_ENDPOINT` | The region endpoint from step 3 |
| `ARTIFACT_SIGNING_ACCOUNT` | The Artifact Signing account name |
| `ARTIFACT_SIGNING_PROFILE` | The certificate profile name |

Setting `ARTIFACT_SIGNING_ACCOUNT` turns signing on for every Release run.

Any branch that can run the Release workflow can sign with this identity. To
limit that, set the environment's deployment branches and tags to the `v*` tag
pattern and `main`. Dry runs from other branches then fail before they start.

## Checking a signed build

`gh workflow run release.yml --ref main` builds without publishing. Its "Check the
Authenticode signatures" step prints each file's signer. The run's
`EternalMonitor-Setup` artifact shows the same name under Properties, Digital
Signatures on Windows.

## Signing a local build

`-SignCommand` takes any command Inno Setup can run once per file. Inno replaces
`$f` with the quoted file path and `$q` with a quote. Use single quotes so
PowerShell leaves `$f` alone. With a test certificate:

```powershell
.\scripts\build-installer.ps1 -SignCommand 'signtool sign /fd SHA256 /f $qC:\certs\test.pfx$q /p <password> $f'
```

The Artifact Signing command that `release.yml` builds also works locally after
`az login` as someone with the Certificate Profile Signer role.

## Updating the pinned packages

`release.yml` downloads `Microsoft.ArtifactSigning.Client` and
`Microsoft.Windows.SDK.BuildTools` from NuGet and checks each package's SHA-256.
To move to a newer version, download its `.nupkg` from nuget.org, run
`Get-FileHash <file> -Algorithm SHA256`, and update both the version and the hash
in the workflow's `env` block.

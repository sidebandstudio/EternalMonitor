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
   `eternalmonitor-release-signing`. The app needs no client secret.
8. Under the app's Certificates & secrets, Federated credentials, add a
   credential with the scenario Other issuer:

   | Field | Value |
   | --- | --- |
   | Issuer | `https://token.actions.githubusercontent.com` |
   | Subject identifier | `repo:sidebandstudio@322977961/EternalMonitor@1199998561:environment:release` |
   | Audience | `api://AzureADTokenExchange` |

   This repository's OIDC tokens use GitHub's
   [immutable subject](https://learn.microsoft.com/en-us/entra/workload-id/workload-identities-github-immutable-subjects),
   which carries the owner and repository IDs. The GitHub Actions scenario
   builds a subject from names alone, and Azure rejects the login when the two
   don't match. `gh api repos/sidebandstudio/EternalMonitor/actions/oidc/customization/sub`
   prints the prefix GitHub uses.
9. On the Artifact Signing account's Access control (IAM), assign the Artifact
   Signing Certificate Profile Signer role to that app.

## GitHub settings

Azure trusts any job that runs in the `release` environment, so lock the
environment down before adding the variable that turns signing on.

1. Under Settings, Environments, `release` (create it if it isn't there), set
   Deployment branches and tags to Selected branches and tags. Add the branch
   `main` and the tag pattern `v*`.
2. Under Settings, Rules, Rulesets, add a tag ruleset. Set Enforcement status
   to Active (new rulesets start Disabled), target tags matching `v*`, tick
   Restrict creations, Restrict updates and Restrict deletions, and put only
   the people who cut releases on the bypass list. Without it, anyone who can
   push a tag starting with `v` can point it at unreviewed code and reach the
   signing step.
3. Optionally add a required reviewer to the environment, so each signing run
   waits for an approval.

The Release job only asks for the environment on `main` and `v*` tags. Dry
runs from other branches still work. They run without the environment, can't
log in, and build unsigned. Keep `release` out of every other workflow, because
the Azure trust covers whatever job uses it.

Then add these to the `release` environment:

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

Setting `ARTIFACT_SIGNING_ACCOUNT` turns signing on for Release runs from
`main` and `v*` tags.

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

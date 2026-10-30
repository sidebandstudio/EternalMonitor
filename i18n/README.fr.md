<div align="center">

[English](../README.md) · [Español](README.es.md) · [العربية](README.ar.md) · [简体中文](README.zh-Hans.md) · [हिन्दी](README.hi.md) · [Русский](README.ru.md) · [日本語](README.ja.md) · [Português (Brasil)](README.pt-BR.md) · **Français** · [Deutsch](README.de.md) · [한국어](README.ko.md)

*En cas de divergence avec le [README anglais](../README.md), celui-ci fait foi.*

<img src="../docs/assets/icon.png" width="88" height="88" alt="Logo EternalMonitor">

# EternalMonitor

Affichez votre bureau Windows sur un iPad en mode recopie ou écran étendu via Wi-Fi ou USB,<br>
et contrôlez le PC au toucher, avec un clavier ou avec Apple Pencil.

[![Release](https://img.shields.io/github/v/release/sidebandstudio/EternalMonitor?label=release&labelColor=111&color=e8ff47)](https://github.com/sidebandstudio/EternalMonitor/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/sidebandstudio/EternalMonitor/ci.yml?branch=main&label=CI&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-e8ff47?labelColor=111)](../LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20iPadOS-e8ff47?labelColor=111)](#télécharger)
[![Website](https://img.shields.io/badge/website-eternalmonitor.dev-e8ff47?labelColor=111)](https://eternalmonitor.dev)
[![Discord](https://img.shields.io/badge/discord-Sideband-e8ff47?labelColor=111&logo=discord&logoColor=white)](https://discord.gg/Xc7NykBZ6z)

[Télécharger](#télécharger) · [Guide de configuration](../SETUP.md) · [Problèmes connus](https://eternalmonitor.dev/known-issues.html) · [Site web](https://eternalmonitor.dev) · [Notes de version](../RELEASE_NOTES.md) · [Discord](https://discord.gg/Xc7NykBZ6z)

</div>

<p align="center">
  <img src="../docs/assets/og.png" width="860" alt="L'application Windows EternalMonitor diffuse l'image du PC sur un iPad">
</p>

EternalMonitor diffuse votre bureau Windows sur un iPad et renvoie vos actions au PC. Une petite application Windows capture l'écran et l'encode sur votre carte graphique. L'application iPad décode la vidéo matériellement et l'affiche avec Metal ; chaque toucher, frappe et trait de Pencil est renvoyé au PC. Le logiciel est gratuit, open source sous MIT License et ne nécessite aucun compte.

## Fonctionnalités

<img src="../docs/assets/ipad-app.webp" align="right" width="270" alt="L'écran de connexion de l'application iPad avec Connecter, Scanner le code QR et Rechercher des PC">

- **Étendre ou recopier.** Ajoutez l'iPad comme écran supplémentaire à côté de votre moniteur ou recopiez n'importe quel écran. L'écran supplémentaire disparaît 10 secondes après la déconnexion de l'iPad : une reconnexion rapide le réutilise et Windows ne conserve pas de moniteur fantôme.
- **Wi-Fi ou USB.** Diffusez sur votre réseau domestique ou branchez un câble pour une image plus stable et pour charger l'iPad. Le câble prend le relais d'une session Wi-Fi en cours ; si vous le débranchez, la connexion revient au Wi-Fi.
- **Toucher, clavier et pointeur.** Touchez pour cliquer, faites glisser, faites défiler à deux doigts et maintenez pour un clic droit. Les claviers physiques, pavés tactiles et souris fonctionnent aussi, avec les touches de modification rémanentes et le clic central.
- **Apple Pencil comme un vrai stylet.** Windows reçoit la pression et l'inclinaison via Windows Ink, et le trait apparaît sous la pointe quelle que soit la disposition des écrans. Le **Mode dessin** (Drawing mode) ignore la paume sur la zone de dessin, et Clip Studio Paint fonctionne avec son réglage Tablet PC.
- **Ajuster ou remplir.** **Ajuster** (Fit) montre toute l'image du PC avec des bandes si les proportions diffèrent. **Remplir** (Fill) utilise tout l'écran de l'iPad et coupe les bords qui dépassent.
- **Son du PC.** Le son est lu sur l'iPad en Opus, avec un tampon de gigue et une dissimulation des pertes. Vous pouvez le couper sans arrêter la vidéo.
- **Réparation des pertes sur Wi-Fi.** Si un paquet manque, l'iPad redemande immédiatement ce fragment ; la perte d'un paquet coûte donc rarement une image. Le débit s'adapte entre 4 et 50 Mbps.
- **Votre GPU travaille.** NVIDIA NVENC, AMD AMF ou Intel Quick Sync encodent en H.264 par défaut ou en HEVC si vous le préférez, à 30, 60, 90 ou 120 fps.
- **Jumelage.** La première connexion Wi-Fi demande un code à six chiffres ou la lecture d'un code QR. L'iPad mémorise ensuite le PC. USB ne nécessite pas de code.
- **Récupération et explications.** L'image revient automatiquement après l'écran de verrouillage, un changement d'affichage ou un redémarrage de la diffusion. En cas de problème, les deux applications expliquent ce qui s'est passé et quoi faire ; **Envoyer un avis** (Send feedback) prépare un e-mail au studio avec votre configuration préremplie.
- **Mises à jour en un clic.** Une fenêtre annonce les nouvelles versions. Sur Windows, **Mettre à jour** (Update now) vérifie le téléchargement et l'installe avec une seule invite Windows ; l'application iPad ouvre TestFlight. L'application Windows fonctionne sans fenêtre de console.

<br clear="right">

## Télécharger

| | Configuration requise | Obtenir |
| --- | --- | --- |
| **Windows** | Windows 10 version 1809 ou ultérieure, ou Windows 11, 64 bits, avec carte graphique NVIDIA, AMD ou Intel | [EternalMonitor-Setup.exe](https://github.com/sidebandstudio/EternalMonitor/releases/tag/v0.3.3) (v0.3.3, version stable actuelle) |
| **iPad** | iPadOS 17 ou ultérieur | [TestFlight v0.3.3, build 364](https://testflight.apple.com/join/ppc9QfXc). La bêta publique est ouverte : installez TestFlight, puis ouvrez le lien sur votre iPad. |

Installez les versions Windows et iPad indiquées sur eternalmonitor.dev/download.html. Elles fonctionnent ensemble même si leurs numéros de version diffèrent. Les anciennes versions v0.1 utilisent un autre protocole et ne peuvent pas se connecter à l'application iPad v0.3.

Vous testez l'application ? Signalez les bugs et envoyez vos remarques par le [formulaire de la page de téléchargement](https://eternalmonitor.dev/download.html#feedback), qui envoie un e-mail à hello@sideband.studio, ouvrez une [issue GitHub](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose) ou écrivez-nous sur le [Discord Sideband](https://discord.gg/Xc7NykBZ6z).

Le programme d'installation n'est pas encore signé : Windows SmartScreen demande donc confirmation avant son exécution. Choisissez **Informations complémentaires** (More info), puis **Exécuter quand même** (Run anyway), uniquement après avoir vérifié que le SHA-256 du fichier correspond à celui de la [page des versions](https://github.com/sidebandstudio/EternalMonitor/releases).

Pour USB, il vous faut aussi l'application gratuite [Apple Devices](https://apps.microsoft.com/detail/9np83lwlpz9k) d'Apple, ouverte pendant la diffusion. Windows ne peut accéder à un iPad par câble que par son intermédiaire ; EternalMonitor vous indique si elle est fermée ou absente. Apple Devices nécessite Windows 10 version 22H2 (build 19045) ou Windows 11 : sur une version antérieure de Windows 10, connectez-vous par Wi-Fi.

**Suivez ensuite le [guide de configuration](../SETUP.md).** Il décrit l'installation des deux applications, le jumelage, USB, l'écran étendu et le dessin, avec les noms exacts des boutons affichés.

## État

v0.3.3 corrige **Second écran** (Second screen) sur les PC où il ne démarrait pas, le plus souvent des ordinateurs portables avec cartes graphiques Intel et NVIDIA. L'iPad affiche l'écran principal du PC pendant le démarrage du second écran et l'écran du PC ne clignote plus en boucle. Mettez l'iPad à jour vers TestFlight v0.3.3, build 364, pour l'utiliser avec Windows 0.3.3.

Les diagnostics Windows et iPad comprennent des informations sur les appareils, les erreurs des applications et les statistiques de diffusion. Les rapports contiennent les noms du PC, de l'iPad et des périphériques audio ainsi que les adresses réseau, mais excluent le contenu de l'écran, l'audio et les identifiants de jumelage. Désactivez le partage automatique dans **Paramètres** (Settings). Les diagnostics sont conservés jusqu'à 30 jours et peuvent être supprimés plus tôt. Les diagnostics supprimés ou expirés peuvent rester dans les sauvegardes quotidiennes jusqu'à 7 jours supplémentaires. Consultez [Diagnostics automatiques](../SETUP.md#automatic-diagnostics) et l'[avis de confidentialité](https://eternalmonitor.dev/privacy.html). Sur iPad, ouvrez **Paramètres** (Settings) > **Assistance** (Support) > **Diagnostics** pour copier ou exporter un rapport.

Les vérifications de version ont réussi sur Windows, macOS et les simulateurs iPad. Les problèmes connus figurent sur la page [Problèmes connus](https://eternalmonitor.dev/known-issues.html), et les résultats antérieurs sur appareils physiques restent dans [docs/hardware-verification.md](../docs/hardware-verification.md).

Limites connues :

- La diffusion n'est pas chiffrée. Le jumelage empêche les autres appareils de se connecter ; utilisez donc un réseau de confiance, comme votre Wi-Fi domestique.
- L'application Windows est le seul hôte. L'hôte se compile sur macOS pour le développement, mais y diffuse une mire de test au lieu de votre écran.
- Apple Pencil (USB-C) ne possède pas de capteur de pression ; ses traits gardent donc une largeur constante.

[Problèmes connus](https://eternalmonitor.dev/known-issues.html) répertorie tous les problèmes connus et indique quoi faire pour chacun.

## Fonctionnement

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../docs/assets/readme/how-it-works-dark.svg">
  <img src="../docs/assets/readme/how-it-works-light.svg" width="1120" alt="Le PC Windows capture le bureau, encode la vidéo sur le GPU et l'envoie par Wi-Fi ou USB. L'iPad reconstitue la vidéo, redemande les fragments manquants sur Wi-Fi, la décode avec VideoToolbox et l'affiche avec Metal. Le son du PC est capturé avec WASAPI, encodé en Opus et lu sur l'iPad. Les actions au toucher, au Pencil, au clavier et au pavé tactile sont renvoyées au PC qui les injecte.">
</picture>

L'hôte Windows est écrit en Rust. Il capture le bureau avec DXGI, encode sur le GPU via FFmpeg et envoie chaque image sous forme de fragments UDP numérotés. Sur USB, les mêmes messages passent par un tunnel à trames fourni par le service d'appareils d'Apple. L'application iPad est native en Swift. Elle reconstitue les fragments, décode avec VideoToolbox et affiche avec Metal. Sur Wi-Fi, elle redemande aussi les fragments manquants. Le son du PC est capturé avec WASAPI et envoyé en Opus ; l'iPad masque les pertes audio au lieu de les redemander. Les actions et les rapports du récepteur reviennent par la même session et l'hôte utilise ces rapports pour adapter le débit.

[ARCHITECTURE.md](../ARCHITECTURE.md) décrit la chaîne et le protocole v2 ; [docs/decisions.md](../docs/decisions.md) explique les choix de fonctionnement de chaque partie.

## Compiler depuis les sources

L'espace de travail contient l'hôte Rust (`host/`), le crate de protocole `eternal-wire` (`proto/`) et l'application iPad Swift (`ios/`).

<details>
<summary><b>Hôte Windows</b></summary>

Configuration requise : Windows 10 version 1809 ou ultérieure, Rust 1.98.0 (fixé par `rust-toolchain.toml`, MSVC), un SDK FFmpeg **7.1 shared** et LLVM/libclang pour bindgen.

```powershell
# Point the build at your FFmpeg 7.1 shared SDK (the folder with bin\avcodec-*.dll)
$env:FFMPEG_DIR = "C:\ffmpeg"
cargo build --release -p eternal-host
.\target\release\eternal-host.exe          # optional port argument, default 9876
```

L'hôte est un programme Windows graphique, sans fenêtre de console. Lancé depuis un terminal, il y écrit son journal mais rend immédiatement la main ; `cargo run -p eternal-host` attend sa fin. Avec `ETERNAL_HEADLESS=1` et une sortie redirigée (`> host.log`), il obtient sa propre console : Ctrl+C dans le terminal ne peut pas l'atteindre. Appuyez sur Ctrl+C dans cette console ou utilisez le Gestionnaire des tâches. `scripts\win\Stop-Host.ps1` arrête uniquement les hôtes lancés par `remote.sh run-host`.

`scripts\build-installer.ps1` construit le Setup.exe complet avec Inno Setup et le même `FFMPEG_DIR`. `scripts\package.ps1` construit un zip minimal.

</details>

<details>
<summary><b>Développement sur macOS</b> (sans Windows)</summary>

Sur macOS, l'hôte remplace la capture d'écran par une mire synthétique. Le protocole, l'encodeur, le transport et le superviseur fonctionnent réellement.

```bash
brew install ffmpeg@7 pkgconf xcodegen
export PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig
cargo test --workspace          # unit, golden-vector and synthetic end-to-end tests
ETERNAL_CAPTURE=synthetic cargo run -p eternal-host
```

Si les outils en ligne de commande de Xcode sont le répertoire de développement sélectionné, préfixez les commandes Xcode par `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`.

</details>

<details>
<summary><b>Application iPad</b></summary>

```bash
cd ios
xcodegen generate               # project.yml is the source of truth
xcodebuild test -project EternalMonitor.xcodeproj -scheme EternalMonitor \
  -destination 'platform=iOS Simulator,name=iPad Pro 11-inch (M4)'
```

Pour l'exécuter sur un iPad physique, ouvrez le projet généré dans Xcode et choisissez votre équipe de signature.

</details>

<details>
<summary><b>Tests de bout en bout</b></summary>

```bash
./scripts/e2e_ios.sh              # synthetic host to the iPad simulator, H.264
EM_CODEC=hevc ./scripts/e2e_ios.sh
scripts/test_ios.sh               # iPad unit and UI tests
scripts/e2e_matrix.sh             # UDP, loss, burst, USB, audio, pairing, input, lifecycle
scripts/soak.sh 1800              # 30-minute frame-rate and memory check
```

Chaque ligne vérifie la fréquence d'images décodées (au moins 55 fps), les pertes, les réparations et les pixels affichés. `e2e_matrix.sh --real` exécute les lignes de matériel Windows via le lanceur dans `scripts/win/` ; lisez d'abord [scripts/win/README.md](../scripts/win/README.md). CI exécute les suites Rust sur Linux, macOS et Windows, les tests iPad et un test système de diffusion pour chaque pull request.

</details>

<details>
<summary><b>Variables d'environnement de l'hôte</b></summary>

| Variable | Effet |
| --- | --- |
| `ETERNAL_ENCODER` | Forcer un encodeur (`h264_nvenc`, `h264_amf`, `h264_qsv`, `libx264`) |
| `ETERNAL_HEVC` | `1` ou `0` remplace la préférence HEVC |
| `ETERNAL_FPS` | Remplacer la fréquence d'images de l'hôte ; la préférence de l'iPad reste la limite |
| `ETERNAL_INPUT` | Entrée de l'encodeur : `yuv420` (par défaut), `auto` ou `bgra` |
| `ETERNAL_MAX_DGRAM` | Taille des datagrammes média, de 576 à 1400 octets |
| `ETERNAL_CAPTURE` | `synthetic` remplace DXGI par une mire générée |
| `ETERNAL_AUDIO` | `synthetic` sélectionne la source audio de test déterministe |
| `ETERNAL_HEADLESS` | `1` exécute sans fenêtre ; Ctrl+C arrête proprement |
| `ETERNAL_ABR` | `0` désactive le débit adaptatif |
| `ETERNAL_VDD_TIMEOUT_SECS` | Délai d'attente de connexion de l'écran virtuel (30 par défaut) |
| `ETERNAL_AMF_DIAG` | `1` écrit les diagnostics du flux binaire AMF |
| `ETERNAL_LEGACY_PTS` | `1` rétablit les horodatages par compteur d'images précédents |
| `ETERNAL_DROP`, `ETERNAL_REORDER`, `ETERNAL_JITTER_MS` | Tests uniquement : injecter des pertes, un réordonnancement ou de la gigue |
| `ETERNAL_USB_DIRECT` | Tests uniquement : point de terminaison TCP à la place du tunnel USB |

</details>

## Documentation

Ces documents sont en anglais.

| Document | Contenu |
| --- | --- |
| [SETUP.md](../SETUP.md) | Configuration pas à pas pour tous, y compris USB et le dessin |
| [Problèmes connus](https://eternalmonitor.dev/known-issues.html) | Problèmes connus et solutions pour chacun (`docs/known-issues.html`) |
| [Confidentialité](https://eternalmonitor.dev/privacy.html) | Données envoyées par les applications, raisons, durée de conservation et désactivation (`docs/privacy.html`) |
| [ARCHITECTURE.md](../ARCHITECTURE.md) | Chaîne, protocole v2 et composants |
| [docs/decisions.md](../docs/decisions.md) | Raisons des choix, avec mesures |
| [docs/pencil-drawing.md](../docs/pencil-drawing.md) | Apple Pencil, Windows Ink et Clip Studio Paint |
| [docs/pairing.md](../docs/pairing.md), [docs/keyboard-input.md](../docs/keyboard-input.md), [docs/host-settings.md](../docs/host-settings.md), [docs/audio-codec.md](../docs/audio-codec.md) | Détails des fonctionnalités |
| [docs/hardware-verification.md](../docs/hardware-verification.md) | Résultats des tests de version et liste de vérification pratique |
| [docs/beta-testing.md](../docs/beta-testing.md) | Tester une bêta : TestFlight, signature et éléments à tester |
| [docs/design.md](../docs/design.md) | Apparence commune des applications et du site |
| [RELEASE_NOTES.md](../RELEASE_NOTES.md) | Modifications de chaque version |

## Contribuer

Les contributions sont bienvenues, du transport et des encodeurs au rendu et à la documentation. Lisez [CONTRIBUTING.md](../CONTRIBUTING.md) avant d'ouvrir une pull request. Pour discuter d'abord d'une idée, posez votre question sur le [Discord Sideband](https://discord.gg/Xc7NykBZ6z).

## Contact

Le [Discord Sideband](https://discord.gg/Xc7NykBZ6z) est destiné aux questions, à l'aide à la configuration et aux idées. Vous pouvez aussi écrire à [hello@sideband.studio](mailto:hello@sideband.studio).

Vous avez trouvé un bug ? [Ouvrez une issue](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose) en indiquant les versions des deux applications et le journal de l'hôte. Signalez les problèmes de sécurité en privé comme indiqué dans [SECURITY.md](../SECURITY.md), pas sur Discord ni dans une issue.

## Licence

EternalMonitor est publié sous [MIT License](../LICENSE), © 2026 Sideband Studio. Le programme d'installation Windows inclut aussi des composants tiers sous leurs propres licences, dont [Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver) et FFmpeg.

Créé par [Sideband Studio](https://sideband.studio).

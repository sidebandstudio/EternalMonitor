<div align="center">

[English](../README.md) · [Español](README.es.md) · [العربية](README.ar.md) · [简体中文](README.zh-Hans.md) · [हिन्दी](README.hi.md) · [Русский](README.ru.md) · [日本語](README.ja.md) · [Português (Brasil)](README.pt-BR.md) · **Français** · [Deutsch](README.de.md) · [한국어](README.ko.md)

*En cas de divergence avec le [README anglais](../README.md), celui-ci fait foi.*

<img src="../docs/assets/icon.png" width="88" height="88" alt="Logo EternalMonitor">

# EternalMonitor

Un second écran gratuit et open source pour Windows.<br>
Connectez votre iPad par Wi-Fi ou USB, avec prise en charge du toucher et d'Apple Pencil.

[![Release](https://img.shields.io/github/v/release/sidebandstudio/EternalMonitor?label=release&labelColor=111&color=e8ff47)](https://github.com/sidebandstudio/EternalMonitor/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/sidebandstudio/EternalMonitor/ci.yml?branch=main&label=CI&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-e8ff47?labelColor=111)](../LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20iPadOS-e8ff47?labelColor=111)](#download)
[![Website](https://img.shields.io/badge/website-eternalmonitor.dev-e8ff47?labelColor=111)](https://eternalmonitor.dev)
[![Discord](https://img.shields.io/badge/discord-Sideband-e8ff47?labelColor=111&logo=discord&logoColor=white)](https://discord.gg/Xc7NykBZ6z)

[Télécharger](#download) · [Guide de configuration](../SETUP.md) · [Site web](https://eternalmonitor.dev) · [Feuille de route](../ROADMAP.md) · [Notes de version](../RELEASE_NOTES.md) · [Discord](https://discord.gg/Xc7NykBZ6z)

</div>

<p align="center">
  <img src="../docs/assets/readme-demo.webp" width="860" alt="Un PC Windows et un iPad. Sur le PC, la fenêtre EternalMonitor passe l'iPad en Second écran, puis une fenêtre est glissée du moniteur vers l'iPad. Un Apple Pencil dessine ensuite des brins d'herbe sur une peinture sur l'iPad, avec des traits épais à la pression et fins au relâchement.">
</p>

EternalMonitor transforme un iPad en écran pour votre PC Windows. Il est gratuit, open source sous licence MIT et ne nécessite aucun compte. Le PC diffuse jusqu'à 120 fps, avec encodage matériel sur les cartes graphiques compatibles et décodage matériel sur l'iPad. Chaque toucher, frappe et trait de Pencil est renvoyé au PC.

<a id="download"></a>

## Démarrage rapide

[![Télécharger pour Windows](https://img.shields.io/badge/Download_for_Windows-v0.3.3-e8ff47?style=for-the-badge&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/releases/download/v0.3.3/EternalMonitor-Setup.exe)
[![Obtenir l'application iPad sur TestFlight](https://img.shields.io/badge/iPad_app-TestFlight-e8ff47?style=for-the-badge&labelColor=111)](https://testflight.apple.com/join/ppc9QfXc)

1. **Installez l'application Windows.** Exécutez [EternalMonitor-Setup.exe](https://github.com/sidebandstudio/EternalMonitor/releases/download/v0.3.3/EternalMonitor-Setup.exe) sur Windows 10 (version 1809 ou ultérieure) ou Windows 11, 64 bits, avec une carte graphique NVIDIA, AMD ou Intel. Si Windows SmartScreen vous le demande, choisissez **Informations complémentaires**, puis **Exécuter quand même**. La [page de la version](https://github.com/sidebandstudio/EternalMonitor/releases/tag/v0.3.3) indique le SHA-256 du fichier.
2. **Installez l'application iPad.** Sur un iPad avec iPadOS 17 ou ultérieur, installez **TestFlight** depuis l'App Store, puis ouvrez le [lien TestFlight d'EternalMonitor](https://testflight.apple.com/join/ppc9QfXc) sur l'iPad et touchez **Installer**.
3. **Connectez-vous.** Les deux applications ouvertes sur le même réseau, touchez votre PC sur l'iPad ou scannez le code QR affiché par le PC. Pour USB sous Windows 10 22H2 ou Windows 11, branchez le câble et gardez ouverte sur le PC l'application gratuite [Apple Devices](https://apps.microsoft.com/detail/9np83lwlpz9k) d'Apple.

Le [guide de configuration](../SETUP.md) décrit chaque étape avec les noms exacts des boutons. La [page de téléchargement](https://eternalmonitor.dev/download.html) indique les versions Windows et iPad compatibles entre elles.

Si EternalMonitor vous est utile, une étoile aide d'autres personnes à le trouver.

## Fonctionnalités

<img src="../docs/assets/ipad-app.webp" align="right" width="270" alt="L'écran de connexion de l'application iPad avec Connecter, Scanner le code QR et Rechercher des PC">

- **Étendre ou recopier.** Ajoutez l'iPad comme second écran à côté de votre moniteur ou recopiez n'importe quel écran. Le second écran reste 10 secondes après la déconnexion de l'iPad : une reconnexion rapide le réutilise et Windows ne conserve pas de moniteur fantôme.
- **Wi-Fi ou USB.** Diffusez sur votre réseau domestique ou branchez un câble pour une image plus stable, qui peut aussi charger l'iPad depuis un port USB-C. Le câble prend le relais d'une session Wi-Fi en cours ; si vous le débranchez, la connexion revient au Wi-Fi.
- **Apple Pencil comme un vrai stylet.** Windows reçoit la pression et l'inclinaison via Windows Ink, et le trait apparaît sous la pointe quelle que soit la disposition des écrans. Le **Mode dessin** (Drawing mode) ignore la paume sur la zone de dessin, et Clip Studio Paint fonctionne avec son réglage Tablet PC.
- **Toucher, clavier et pointeur.** Touchez pour cliquer, faites glisser, faites défiler à deux doigts et maintenez pour un clic droit. Les claviers physiques, pavés tactiles et souris fonctionnent aussi, avec les touches de modification rémanentes et le clic central.
- **Encodage matériel.** Les cartes graphiques NVIDIA, AMD et Intel compatibles encodent la diffusion à 30, 60, 90 ou 120 fps.
- **Réparation des pertes sur Wi-Fi.** Si un paquet manque, l'iPad le redemande immédiatement ; la perte d'un paquet coûte donc rarement une image. Le débit s'adapte à votre réseau.
- **Son du PC.** Le son est lu sur l'iPad et vous pouvez le couper sans arrêter la vidéo.
- **Ajuster ou remplir.** **Ajuster** (Fit) montre toute l'image du PC avec des bandes si les proportions diffèrent. **Remplir** (Fill) utilise tout l'écran de l'iPad et coupe les bords qui dépassent.
- **Jumelage.** La première connexion Wi-Fi demande un code à six chiffres ou la lecture d'un code QR. L'iPad mémorise ensuite le PC. USB ne nécessite pas de code.
- **Récupération et explications.** L'image revient automatiquement après l'écran de verrouillage, un changement d'affichage ou un redémarrage de la diffusion. Les deux applications expliquent ce qui s'est passé et quoi faire ; **Envoyer un avis** (Send feedback) prépare un e-mail au studio avec votre configuration préremplie.
- **Mises à jour en un clic.** Une fenêtre annonce les nouvelles versions. Sur Windows, **Mettre à jour** (Update now) vérifie le téléchargement et l'installe avec une seule invite Windows ; sur l'iPad, il ouvre TestFlight. L'application Windows fonctionne sans fenêtre de console.

Ce README et le [site web](https://eternalmonitor.dev) sont disponibles en 11 langues.

<br clear="right">

## En savoir plus

- [Utiliser un iPad comme second écran pour Windows](https://eternalmonitor.dev/ipad-second-monitor-windows.html) : le guide complet.
- [Compatibilité](https://eternalmonitor.dev/compatibility.html) : les PC et iPad utilisés avec EternalMonitor.
- [Alternatives](https://eternalmonitor.dev/alternatives.html) : comparaison d'EternalMonitor avec d'autres façons d'utiliser un iPad comme écran.
- [Feuille de route](../ROADMAP.md) : les prochaines étapes.

**Partagez votre configuration.** Indiquez-nous quels PC et iPad vous utilisez dans le [rapport de compatibilité](https://github.com/sidebandstudio/EternalMonitor/issues/new?template=compatibility.yml), ou publiez-les sur le [Discord Sideband](https://discord.gg/Xc7NykBZ6z). Avec votre permission, ils figureront sur la page de compatibilité.

## État

La version actuelle est v0.3.3 pour Windows et v0.3.3 (build 364) pour iPad. Elle apporte **Second écran** (Second screen) à davantage de portables avec cartes graphiques Intel et NVIDIA : l'iPad affiche immédiatement l'écran principal du PC, puis passe au second écran lorsque Windows l'ajoute. [RELEASE_NOTES.md](../RELEASE_NOTES.md) couvre chaque version et [ROADMAP.md](../ROADMAP.md) les prochaines étapes.

Les vérifications de la version v0.3.3 ont réussi sur Windows, macOS et les simulateurs iPad. [docs/hardware-verification.md](../docs/hardware-verification.md) consigne les résultats des tests pratiques sur un vrai PC et un vrai iPad depuis v0.3.0.

Les deux applications partagent des diagnostics pour trouver et corriger les problèmes : informations sur les appareils, erreurs des applications et statistiques de diffusion, notamment les noms du PC, de l'iPad et des périphériques audio ainsi que les adresses réseau. Les diagnostics ne contiennent jamais le contenu de l'écran, l'audio ni les identifiants de jumelage. Désactivez le partage dans Paramètres. [Diagnostics automatiques](../SETUP.md#automatic-diagnostics) et l'[avis de confidentialité](https://eternalmonitor.dev/privacy.html) précisent ce qui est envoyé et la durée de conservation. Sur iPad, ouvrez Paramètres > Assistance > Diagnostics pour copier ou exporter un rapport.

## Fonctionnement

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../docs/assets/readme/how-it-works-dark.svg">
  <img src="../docs/assets/readme/how-it-works-light.svg" width="1120" alt="Le PC Windows capture le bureau, encode la vidéo sur le GPU et l'envoie par Wi-Fi ou USB. L'iPad reconstitue la vidéo, redemande les fragments manquants sur Wi-Fi, la décode avec VideoToolbox et l'affiche avec Metal. Le son du PC est capturé avec WASAPI, encodé en Opus et lu sur l'iPad. Les actions au toucher, au Pencil, au clavier et au pavé tactile sont renvoyées au PC qui les injecte.">
</picture>

L'hôte Windows est écrit en Rust. Il capture le bureau avec DXGI, encode via FFmpeg avec NVIDIA NVENC, AMD AMF ou Intel Quick Sync sur les cartes graphiques compatibles (H.264 par défaut, ou HEVC) et envoie chaque image sous forme de fragments UDP numérotés. Sur USB, les mêmes messages passent par un tunnel à trames fourni par le service d'appareils d'Apple. L'application iPad est native en Swift. Elle reconstitue les fragments, décode avec VideoToolbox et affiche avec Metal. Sur Wi-Fi, elle redemande aussi les fragments manquants. Le son du PC est capturé avec WASAPI et envoyé en Opus ; l'iPad le lit via un tampon de gigue et masque les pertes audio au lieu de les redemander. Les actions et les rapports du récepteur reviennent par la même session et l'hôte utilise ces rapports pour adapter le débit entre 4 et 50 Mbps.

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
| [Tests de version](../docs/beta-testing.md) | Publier une build via TestFlight : signature et éléments à tester |
| [docs/design.md](../docs/design.md) | Apparence commune des applications et du site |
| [RELEASE_NOTES.md](../RELEASE_NOTES.md) | Modifications de chaque version |
| [ROADMAP.md](../ROADMAP.md) | Les prochaines étapes |

## Contribuer

Toutes les contributions sont les bienvenues : issues, idées, corrections, documentation, traductions et nouvelles fonctionnalités. Les pull requests sont bienvenues, petites ou grandes. [CONTRIBUTING.md](../CONTRIBUTING.md) explique comment en envoyer une et propose de bonnes premières contributions, et [ROADMAP.md](../ROADMAP.md) présente les travaux plus importants à venir. Vos questions et idées sont les bienvenues sur le [Discord Sideband](https://discord.gg/Xc7NykBZ6z).

## Contact

Le [Discord Sideband](https://discord.gg/Xc7NykBZ6z) est destiné aux questions, à l'aide à la configuration et aux idées. Vous pouvez aussi écrire à [hello@sideband.studio](mailto:hello@sideband.studio).

Vous avez trouvé un bug ? [Ouvrez une issue](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose) en indiquant les versions des deux applications et le journal de l'hôte, ou utilisez le [formulaire de retour sur la page de téléchargement](https://eternalmonitor.dev/download.html#feedback), qui envoie un e-mail au studio. Signalez les problèmes de sécurité en privé comme indiqué dans [SECURITY.md](../SECURITY.md), pas sur Discord ni dans une issue.

## Licence

EternalMonitor est publié sous [MIT License](../LICENSE), © 2026 Sideband Studio. Le programme d'installation Windows inclut aussi des composants tiers sous leurs propres licences, dont [Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver) et FFmpeg.

Créé par [Sideband Studio](https://sideband.studio).

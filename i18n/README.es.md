<div align="center">

[English](../README.md) · **Español** · [العربية](README.ar.md) · [简体中文](README.zh-Hans.md) · [हिन्दी](README.hi.md) · [Русский](README.ru.md) · [日本語](README.ja.md) · [Português (Brasil)](README.pt-BR.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [한국어](README.ko.md)

*Si esta página y el [README en inglés](../README.md) difieren, la versión en inglés es la vigente.*

<img src="../docs/assets/icon.png" width="88" height="88" alt="Logotipo de EternalMonitor">

# EternalMonitor

Un segundo monitor gratis y de código abierto para Windows.<br>
Conecta tu iPad por Wi-Fi o USB, con soporte táctil y Apple Pencil.

[![Release](https://img.shields.io/github/v/release/sidebandstudio/EternalMonitor?label=release&labelColor=111&color=e8ff47)](https://github.com/sidebandstudio/EternalMonitor/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/sidebandstudio/EternalMonitor/ci.yml?branch=main&label=CI&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-e8ff47?labelColor=111)](../LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20iPadOS-e8ff47?labelColor=111)](#download)
[![Website](https://img.shields.io/badge/website-eternalmonitor.dev-e8ff47?labelColor=111)](https://eternalmonitor.dev)
[![Discord](https://img.shields.io/badge/discord-Sideband-e8ff47?labelColor=111&logo=discord&logoColor=white)](https://discord.gg/Xc7NykBZ6z)

[Descargar](#download) · [Guía de configuración](../SETUP.md) · [Sitio web](https://eternalmonitor.dev) · [Hoja de ruta](../ROADMAP.md) · [Notas de la versión](../RELEASE_NOTES.md) · [Discord](https://discord.gg/Xc7NykBZ6z)

</div>

<p align="center">
  <img src="../docs/assets/readme-demo.webp" width="860" alt="Un PC con Windows y un iPad. En el PC, la ventana de EternalMonitor cambia el iPad a Segunda pantalla y una ventana se arrastra del monitor al iPad. Luego, un Apple Pencil dibuja hojas de hierba en una pintura en el iPad, con trazos gruesos al presionar y finos al levantarlo.">
</p>

EternalMonitor convierte un iPad en un monitor para tu PC con Windows. Es gratis, de código abierto bajo la licencia MIT y no requiere una cuenta. El PC transmite hasta a 120 fps, con codificación por hardware en tarjetas gráficas compatibles y decodificación por hardware en el iPad. Cada toque, pulsación de tecla y trazo del Pencil vuelve al PC.

<a id="download"></a>

## Inicio rápido

[![Descargar para Windows](https://img.shields.io/badge/Download_for_Windows-v0.3.3-e8ff47?style=for-the-badge&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/releases/download/v0.3.3/EternalMonitor-Setup.exe)
[![Obtener la aplicación para iPad en TestFlight](https://img.shields.io/badge/iPad_app-TestFlight-e8ff47?style=for-the-badge&labelColor=111)](https://testflight.apple.com/join/ppc9QfXc)

1. **Instala la aplicación de Windows.** Ejecuta [EternalMonitor-Setup.exe](https://github.com/sidebandstudio/EternalMonitor/releases/download/v0.3.3/EternalMonitor-Setup.exe) en Windows 10 (versión 1809 o posterior) o Windows 11 de 64 bits, con gráficos NVIDIA, AMD o Intel. Si Windows SmartScreen te pregunta, elige **Más información** y luego **Ejecutar de todas formas**. La [página de la versión](https://github.com/sidebandstudio/EternalMonitor/releases/tag/v0.3.3) indica el SHA-256 del archivo.
2. **Instala la aplicación para iPad.** En un iPad con iPadOS 17 o posterior, instala **TestFlight** desde App Store. Luego abre el [enlace de EternalMonitor en TestFlight](https://testflight.apple.com/join/ppc9QfXc) en el iPad y toca **Instalar**.
3. **Conecta.** Con ambas aplicaciones abiertas en la misma red, toca tu PC en el iPad o escanea el código QR que muestra el PC. Para usar USB en Windows 10 22H2 o Windows 11, conecta el cable y mantén abierta en el PC la aplicación gratuita [Apple Devices](https://apps.microsoft.com/detail/9np83lwlpz9k) de Apple.

La [guía de configuración](../SETUP.md) explica cada paso con los nombres exactos de los botones. La [página de descarga](https://eternalmonitor.dev/download.html) indica qué versiones de Windows y iPad funcionan juntas.

Si EternalMonitor te resulta útil, una estrella ayuda a otras personas a encontrarlo.

## Funciones

<img src="../docs/assets/ipad-app.webp" align="right" width="270" alt="La pantalla de conexión de la aplicación para iPad con Conectar, Escanear código QR y Buscar PC">

- **Extender o duplicar.** Agrega el iPad como segunda pantalla junto a tu monitor o duplica cualquiera de tus pantallas. La segunda pantalla permanece durante 10 segundos después de que se desconecta el iPad, por lo que una reconexión rápida la reutiliza y Windows no conserva un monitor fantasma.
- **Wi-Fi o USB.** Transmite por tu red doméstica o conecta un cable para obtener una imagen más estable, que también puede cargar el iPad desde un puerto USB-C. El cable toma el control de una sesión Wi-Fi en curso y, al desconectarlo, se vuelve a usar Wi-Fi.
- **Apple Pencil como lápiz real.** Windows recibe la presión y la inclinación mediante Windows Ink, y el trazo aparece bajo la punta en cualquier disposición de monitores. El **Modo de dibujo** (Drawing mode) ignora la palma sobre el lienzo y Clip Studio Paint funciona con su ajuste Tablet PC.
- **Pantalla táctil, teclado y puntero.** Toca para hacer clic, arrastra, desplázate con dos dedos y mantén presionado para hacer clic derecho. También funcionan teclados físicos, trackpads y ratones, con modificadores persistentes y clic central.
- **Codificación por hardware.** Los gráficos NVIDIA, AMD e Intel compatibles codifican la transmisión a 30, 60, 90 o 120 fps.
- **Reparación de pérdidas en Wi-Fi.** Cuando falta un paquete, el iPad lo solicita de inmediato, por lo que rara vez se pierde un fotograma por un paquete. La tasa de bits se adapta a tu red.
- **Audio del PC.** El sonido se reproduce en el iPad y puedes silenciarlo sin detener el video.
- **Ajustar o rellenar.** **Ajustar** (Fit) muestra toda la imagen del PC, con barras donde las proporciones difieren. **Rellenar** (Fill) usa toda la pantalla del iPad y recorta los bordes que no caben.
- **Vinculación.** La primera conexión por Wi-Fi pide un código de seis dígitos o escanear un código QR. Después, el iPad recuerda el PC. USB no necesita código.
- **Se recupera y explica.** La imagen vuelve por sí sola después de la pantalla de bloqueo, un cambio de pantalla o un reinicio de la transmisión. Ambas aplicaciones indican qué pasó y qué hacer, y **Enviar comentarios** (Send feedback) redacta un correo al estudio con los datos de tu configuración.
- **Actualizaciones con un clic.** Una ventana emergente te avisa cuando hay una nueva versión. En Windows, **Actualizar ahora** (Update now) comprueba la descarga y la instala con una sola confirmación de Windows; en el iPad, abre TestFlight. La aplicación de Windows se ejecuta sin una ventana de consola.

Este README y el [sitio web](https://eternalmonitor.dev) están disponibles en 11 idiomas.

<br clear="right">

## Más información

- [Usa un iPad como segundo monitor para Windows](https://eternalmonitor.dev/ipad-second-monitor-windows.html): la guía completa.
- [Compatibilidad](https://eternalmonitor.dev/compatibility.html): PC y iPad en los que se usa EternalMonitor.
- [Alternativas](https://eternalmonitor.dev/alternatives.html): cómo se compara EternalMonitor con otras formas de usar un iPad como pantalla.
- [Hoja de ruta](../ROADMAP.md): lo que viene después.

**Comparte tu configuración.** Cuéntanos qué PC y iPad usas mediante el [informe de compatibilidad](https://github.com/sidebandstudio/EternalMonitor/issues/new?template=compatibility.yml), o publícalo en el [Discord de Sideband](https://discord.gg/Xc7NykBZ6z). Con tu permiso, aparecerá en la página de compatibilidad.

## Estado

La versión actual es v0.3.3 para Windows y v0.3.3 (build 364) para iPad. Lleva **Segunda pantalla** (Second screen) a más portátiles con gráficos Intel y NVIDIA: el iPad muestra de inmediato la pantalla principal del PC y cambia a la segunda pantalla cuando Windows la agrega. [RELEASE_NOTES.md](../RELEASE_NOTES.md) cubre todas las versiones y [ROADMAP.md](../ROADMAP.md), lo que viene después.

Las comprobaciones de la versión v0.3.3 se aprobaron en Windows, macOS y simuladores de iPad. [docs/hardware-verification.md](../docs/hardware-verification.md) registra los resultados de las pruebas prácticas con un PC y un iPad reales desde v0.3.0.

Ambas aplicaciones comparten diagnósticos para detectar y corregir problemas: detalles del dispositivo, errores de la aplicación y estadísticas de transmisión, incluidos nombres del PC y del iPad, nombres de dispositivos de audio y direcciones de red. Los diagnósticos nunca incluyen contenido de la pantalla, audio ni credenciales de vinculación. Desactiva el envío en Ajustes. [Diagnósticos automáticos](../SETUP.md#automatic-diagnostics) y el [aviso de privacidad](https://eternalmonitor.dev/privacy.html) detallan qué se envía y cuánto tiempo se conserva. En el iPad, abre Ajustes > Soporte > Diagnósticos para copiar o exportar un informe.

## Cómo funciona

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../docs/assets/readme/how-it-works-dark.svg">
  <img src="../docs/assets/readme/how-it-works-light.svg" width="1120" alt="El PC con Windows captura el escritorio, codifica el video en la GPU y lo envía por Wi-Fi o USB. El iPad recompone el video, solicita los fragmentos que faltan por Wi-Fi, lo decodifica con VideoToolbox y lo muestra con Metal. El audio del PC se captura con WASAPI, se codifica como Opus y se reproduce en el iPad. Las acciones táctiles, de Pencil, teclado y trackpad vuelven al PC, que las inyecta.">
</picture>

El host de Windows está escrito en Rust. Captura el escritorio con DXGI, codifica mediante FFmpeg con NVIDIA NVENC, AMD AMF o Intel Quick Sync en hardware gráfico compatible (H.264 de forma predeterminada, o HEVC) y envía cada fotograma como fragmentos UDP numerados. Por USB, los mismos mensajes viajan por un túnel con enmarcado proporcionado por el servicio de dispositivos de Apple. La aplicación del iPad es nativa de Swift. Recompone los fragmentos, decodifica con VideoToolbox y renderiza con Metal. En Wi-Fi también solicita los fragmentos que faltan. El audio del PC se captura con WASAPI y se envía como Opus, y el iPad lo reproduce mediante un búfer de fluctuación y oculta el audio perdido en lugar de solicitarlo de nuevo. Las acciones y los informes del receptor vuelven por la misma sesión, y el host usa esos informes para adaptar la tasa de bits entre 4 y 50 Mbps.

[ARCHITECTURE.md](../ARCHITECTURE.md) describe el flujo y el protocolo v2, y [docs/decisions.md](../docs/decisions.md) explica por qué cada parte funciona como lo hace.

## Compilar desde el código fuente

El espacio de trabajo contiene el host Rust (`host/`), el crate de protocolo `eternal-wire` (`proto/`) y la aplicación Swift para iPad (`ios/`).

<details>
<summary><b>Host de Windows</b></summary>

Requisitos: Windows 10 versión 1809 o posterior, Rust 1.98.0 (fijado por `rust-toolchain.toml`, MSVC), un SDK de FFmpeg **7.1 shared** y LLVM/libclang para bindgen.

```powershell
# Point the build at your FFmpeg 7.1 shared SDK (the folder with bin\avcodec-*.dll)
$env:FFMPEG_DIR = "C:\ffmpeg"
cargo build --release -p eternal-host
.\target\release\eternal-host.exe          # optional port argument, default 9876
```

El host es un programa gráfico de Windows, por lo que no abre una ventana de consola. Al iniciarlo desde una terminal, imprime allí su registro, pero el prompt vuelve de inmediato; `cargo run -p eternal-host` espera a que termine. Con `ETERNAL_HEADLESS=1` y la salida redirigida (`> host.log`), obtiene su propia consola, por lo que Ctrl+C en la terminal no llega a él: presiona Ctrl+C en la ventana de esa consola o usa el Administrador de tareas. `scripts\win\Stop-Host.ps1` detiene solo los hosts iniciados por `remote.sh run-host`.

`scripts\build-installer.ps1` compila el Setup.exe completo con Inno Setup y el mismo `FFMPEG_DIR`. `scripts\package.ps1` crea un zip básico.

</details>

<details>
<summary><b>Ciclo de desarrollo en macOS</b> (sin necesidad de Windows)</summary>

En macOS el host sustituye la captura de pantalla por un patrón de prueba sintético. El protocolo, el codificador, el transporte y el supervisor funcionan de verdad.

```bash
brew install ffmpeg@7 pkgconf xcodegen
export PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig
cargo test --workspace          # unit, golden-vector and synthetic end-to-end tests
ETERNAL_CAPTURE=synthetic cargo run -p eternal-host
```

Si las herramientas de línea de comandos de Xcode son el directorio de desarrollador seleccionado, antepone `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer` a los comandos de Xcode.

</details>

<details>
<summary><b>Aplicación para iPad</b></summary>

```bash
cd ios
xcodegen generate               # project.yml is the source of truth
xcodebuild test -project EternalMonitor.xcodeproj -scheme EternalMonitor \
  -destination 'platform=iOS Simulator,name=iPad Pro 11-inch (M4)'
```

Para ejecutarla en un iPad físico, abre el proyecto generado en Xcode y elige tu propio equipo de firma.

</details>

<details>
<summary><b>Pruebas de extremo a extremo</b></summary>

```bash
./scripts/e2e_ios.sh              # synthetic host to the iPad simulator, H.264
EM_CODEC=hevc ./scripts/e2e_ios.sh
scripts/test_ios.sh               # iPad unit and UI tests
scripts/e2e_matrix.sh             # UDP, loss, burst, USB, audio, pairing, input, lifecycle
scripts/soak.sh 1800              # 30-minute frame-rate and memory check
```

Cada fila comprueba la velocidad de fotogramas decodificados (al menos 55 fps), las pérdidas, las reparaciones y los píxeles renderizados. `e2e_matrix.sh --real` ejecuta las filas de hardware de Windows mediante el ejecutor en `scripts/win/`; primero lee [scripts/win/README.md](../scripts/win/README.md). CI ejecuta las suites de Rust en Linux, macOS y Windows, las pruebas del iPad y una prueba de sistema de transmisión en cada pull request.

</details>

<details>
<summary><b>Variables de entorno del host</b></summary>

| Variable | Efecto |
| --- | --- |
| `ETERNAL_ENCODER` | Fuerza un codificador (`h264_nvenc`, `h264_amf`, `h264_qsv`, `libx264`) |
| `ETERNAL_HEVC` | `1` o `0` anula la preferencia de HEVC |
| `ETERNAL_FPS` | Anula la velocidad de fotogramas del host; la preferencia del iPad sigue limitándola |
| `ETERNAL_INPUT` | Entrada del codificador: `automatic` (predeterminada: BGRA para NVIDIA, YUV420 para los demás), `yuv420`, `auto` o `bgra` |
| `ETERNAL_MAX_DGRAM` | Tamaño del datagrama multimedia, de 576 a 1400 bytes |
| `ETERNAL_CAPTURE` | `synthetic` sustituye DXGI por un patrón de prueba generado |
| `ETERNAL_AUDIO` | `synthetic` selecciona la fuente determinista de audio de prueba |
| `ETERNAL_HEADLESS` | `1` ejecuta sin ventana; Ctrl+C cierra correctamente |
| `ETERNAL_ABR` | `0` desactiva la tasa de bits adaptativa |
| `ETERNAL_VDD_TIMEOUT_SECS` | Tiempo de espera para conectar la pantalla virtual (30 de forma predeterminada) |
| `ETERNAL_AMF_DIAG` | `1` escribe diagnósticos del flujo de bits AMF |
| `ETERNAL_LEGACY_PTS` | `1` restablece las marcas de tiempo antiguas basadas en el contador de fotogramas |
| `ETERNAL_DROP`, `ETERNAL_REORDER`, `ETERNAL_JITTER_MS` | Solo para pruebas: inyecta pérdidas, reordenamiento o fluctuación |
| `ETERNAL_USB_DIRECT` | Solo para pruebas: un punto de conexión TCP en lugar del túnel USB |

</details>

## Documentación

| Documento | Contenido |
| --- | --- |
| [SETUP.md](../SETUP.md) | Configuración paso a paso para cualquier persona, incluidos USB y dibujo |
| [Problemas conocidos](https://eternalmonitor.dev/known-issues.html) | Problemas que conocemos y qué hacer en cada caso (`docs/known-issues.html`) |
| [Privacidad](https://eternalmonitor.dev/privacy.html) | Qué envían las aplicaciones, por qué, cuánto tiempo se conserva y cómo desactivarlo (`docs/privacy.html`) |
| [ARCHITECTURE.md](../ARCHITECTURE.md) | El flujo, el protocolo v2 y cada componente |
| [docs/decisions.md](../docs/decisions.md) | Por qué las cosas son como son, con mediciones |
| [docs/pencil-drawing.md](../docs/pencil-drawing.md) | Apple Pencil, Windows Ink y Clip Studio Paint |
| [docs/pairing.md](../docs/pairing.md), [docs/keyboard-input.md](../docs/keyboard-input.md), [docs/host-settings.md](../docs/host-settings.md), [docs/audio-codec.md](../docs/audio-codec.md) | Detalles de las funciones |
| [docs/hardware-verification.md](../docs/hardware-verification.md) | El registro de pruebas de la versión y la lista de comprobación práctica |
| [Pruebas de la versión](../docs/beta-testing.md) | Publicar una compilación mediante TestFlight: firma y qué probar |
| [docs/design.md](../docs/design.md) | La apariencia compartida de las aplicaciones y el sitio web |
| [RELEASE_NOTES.md](../RELEASE_NOTES.md) | Cambios de cada versión |
| [ROADMAP.md](../ROADMAP.md) | Lo que viene después |

## Contribuir

Todas las contribuciones son bienvenidas: issues, ideas, correcciones, documentación, traducciones y nuevas funciones. Los pull requests son bienvenidos, grandes o pequeños. [CONTRIBUTING.md](../CONTRIBUTING.md) explica cómo enviar uno y enumera buenas primeras contribuciones, y [ROADMAP.md](../ROADMAP.md) muestra el trabajo más amplio que queda por hacer. Las preguntas e ideas son bienvenidas en el [Discord de Sideband](https://discord.gg/Xc7NykBZ6z).

## Contacto

El [Discord de Sideband](https://discord.gg/Xc7NykBZ6z) es el lugar para preguntas, ayuda con la configuración e ideas. También puedes escribir a [hello@sideband.studio](mailto:hello@sideband.studio).

¿Encontraste un error? [Abre un issue](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose) con las versiones de ambas aplicaciones y el registro del host, o usa el [formulario de comentarios de la página de descarga](https://eternalmonitor.dev/download.html#feedback), que envía un correo al estudio. Informa los problemas de seguridad en privado como se describe en [SECURITY.md](../SECURITY.md), no en Discord ni en un issue.

## Licencia

EternalMonitor se publica bajo la [MIT License](../LICENSE), © 2026 Sideband Studio. El instalador de Windows también incluye componentes de terceros bajo sus propias licencias, entre ellos [Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver) y FFmpeg.

Creado por [Sideband Studio](https://sideband.studio).

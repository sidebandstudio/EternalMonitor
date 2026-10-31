<div align="center">

[English](../README.md) · **Español** · [العربية](README.ar.md) · [简体中文](README.zh-Hans.md) · [हिन्दी](README.hi.md) · [Русский](README.ru.md) · [日本語](README.ja.md) · [Português (Brasil)](README.pt-BR.md) · [Français](README.fr.md) · [Deutsch](README.de.md) · [한국어](README.ko.md)

*Si esta página y el [README en inglés](../README.md) difieren, la versión en inglés es la vigente.*

<img src="../docs/assets/icon.png" width="88" height="88" alt="Logotipo de EternalMonitor">

# EternalMonitor

Duplica o extiende tu escritorio de Windows en un iPad por Wi-Fi o USB,<br>
y controla el PC con la pantalla táctil, un teclado o Apple Pencil.

[![Release](https://img.shields.io/github/v/release/sidebandstudio/EternalMonitor?label=release&labelColor=111&color=e8ff47)](https://github.com/sidebandstudio/EternalMonitor/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/sidebandstudio/EternalMonitor/ci.yml?branch=main&label=CI&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-e8ff47?labelColor=111)](../LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20iPadOS-e8ff47?labelColor=111)](#descargar)
[![Website](https://img.shields.io/badge/website-eternalmonitor.dev-e8ff47?labelColor=111)](https://eternalmonitor.dev)
[![Discord](https://img.shields.io/badge/discord-Sideband-e8ff47?labelColor=111&logo=discord&logoColor=white)](https://discord.gg/Xc7NykBZ6z)

[Descargar](#descargar) · [Guía de configuración](../SETUP.md) · [Problemas conocidos](https://eternalmonitor.dev/known-issues.html) · [Sitio web](https://eternalmonitor.dev) · [Notas de la versión](../RELEASE_NOTES.md) · [Discord](https://discord.gg/Xc7NykBZ6z)

</div>

<p align="center">
  <img src="../docs/assets/og.png" width="860" alt="La aplicación de EternalMonitor para Windows transmite a un iPad que muestra la imagen del PC">
</p>

EternalMonitor transmite tu escritorio de Windows a un iPad y devuelve tus acciones al PC. Una pequeña aplicación de Windows captura la pantalla y la codifica en la tarjeta gráfica. La aplicación del iPad decodifica el video por hardware y lo dibuja con Metal; cada toque, pulsación de tecla y trazo del Pencil vuelve al PC. Es gratis, de código abierto bajo la MIT License y no requiere una cuenta.

## Funciones

<img src="../docs/assets/ipad-app.webp" align="right" width="270" alt="La pantalla de conexión de la aplicación para iPad con Conectar, Escanear código QR y Buscar PC">

- **Extender o duplicar.** Agrega el iPad como pantalla adicional junto a tu monitor o duplica cualquiera de tus pantallas. La pantalla adicional desaparece 10 segundos después de que se desconecta el iPad, por lo que una reconexión rápida la reutiliza y Windows no conserva un monitor fantasma.
- **Wi-Fi o USB.** Transmite por tu red doméstica o conecta un cable para obtener una imagen más estable y cargar el iPad. El cable toma el control de una sesión Wi-Fi en curso y, al desconectarlo, se vuelve a usar Wi-Fi.
- **Pantalla táctil, teclado y puntero.** Toca para hacer clic, arrastra, desplázate con dos dedos y mantén presionado para hacer clic derecho. También funcionan teclados físicos, trackpads y ratones, con modificadores persistentes y clic central.
- **Apple Pencil como lápiz real.** Windows recibe la presión y la inclinación mediante Windows Ink, y el trazo aparece bajo la punta en cualquier disposición de monitores. El **Modo de dibujo** (Drawing mode) ignora la palma sobre el lienzo y Clip Studio Paint funciona con su ajuste Tablet PC.
- **Ajustar o rellenar.** **Ajustar** (Fit) muestra toda la imagen del PC, con barras donde las proporciones difieren. **Rellenar** (Fill) usa toda la pantalla del iPad y recorta los bordes que no caben.
- **Audio del PC.** El sonido se reproduce en el iPad como Opus, con un búfer de fluctuación y ocultación de pérdidas. Puedes silenciarlo sin detener el video.
- **Reparación de pérdidas en Wi-Fi.** Cuando falta un paquete, el iPad solicita de inmediato ese fragmento de nuevo, por lo que rara vez se pierde un fotograma por un paquete. La tasa de bits se adapta entre 4 y 50 Mbps.
- **Tu GPU hace el trabajo.** NVIDIA NVENC, AMD AMF o Intel Quick Sync codifican en H.264 de forma predeterminada, o en HEVC si lo prefieres, a 30, 60, 90 o 120 fps.
- **Vinculación.** La primera conexión por Wi-Fi pide un código de seis dígitos o escanear un código QR. Después, el iPad recuerda el PC. USB no necesita código.
- **Se recupera y explica.** La imagen vuelve por sí sola después de la pantalla de bloqueo, un cambio de pantalla o un reinicio de la transmisión. Si algo falla, ambas aplicaciones indican qué pasó y qué hacer, y **Enviar comentarios** (Send feedback) redacta un correo al estudio con los datos de tu configuración.
- **Actualizaciones con un clic.** Una ventana emergente te avisa cuando hay una nueva versión. En Windows, **Actualizar ahora** (Update now) comprueba la descarga y la instala con una sola confirmación de Windows; la aplicación del iPad abre TestFlight. La aplicación de Windows se ejecuta sin una ventana de consola.

<br clear="right">

## Descargar

| | Requisitos | Obtener |
| --- | --- | --- |
| **Windows** | Windows 10 versión 1809 o posterior, o Windows 11, de 64 bits, con gráficos NVIDIA, AMD o Intel | [EternalMonitor-Setup.exe](https://github.com/sidebandstudio/EternalMonitor/releases/tag/v0.3.3) (v0.3.3 estable actual) |
| **iPad** | iPadOS 17 o posterior | [TestFlight v0.3.3, build 364](https://testflight.apple.com/join/ppc9QfXc). La beta pública está abierta: instala TestFlight y abre el enlace en tu iPad. |

Instala las versiones de Windows y iPad indicadas en eternalmonitor.dev/download.html. Estas versiones funcionan juntas aunque sus números de versión sean distintos. Las versiones anteriores v0.1 usan otro protocolo y no pueden conectarse a la aplicación v0.3 para iPad.

¿La estás probando? Envía errores y comentarios mediante el [formulario de la página de descarga](https://eternalmonitor.dev/download.html#feedback), que envía un correo a hello@sideband.studio, abre un [issue en GitHub](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose) o cuéntanos en el [Discord de Sideband](https://discord.gg/Xc7NykBZ6z).

El instalador aún no tiene firma de código, así que Windows SmartScreen pide confirmación antes de ejecutarlo. Elige **Más información** (More info) y luego **Ejecutar de todas formas** (Run anyway), solo después de comprobar que el SHA-256 del archivo coincida con el de la [página de versiones](https://github.com/sidebandstudio/EternalMonitor/releases).

Para USB también necesitas la aplicación gratuita [Apple Devices](https://apps.microsoft.com/detail/9np83lwlpz9k) de Apple abierta durante la transmisión. Windows solo puede comunicarse con un iPad por cable mediante esta aplicación, y EternalMonitor te avisa si está cerrada o no está instalada. Apple Devices necesita Windows 10 versión 22H2 (build 19045) o Windows 11, así que en versiones anteriores de Windows 10 conéctate por Wi-Fi.

**Luego, sigue la [guía de configuración](../SETUP.md).** Explica cómo instalar ambas aplicaciones, vincularlas, usar USB, la pantalla extendida y el dibujo, con los nombres exactos de los botones que verás.

## Estado

v0.3.3 corrige **Segunda pantalla** (Second screen) en PC donde no se iniciaba, especialmente en portátiles con gráficos Intel y NVIDIA. El iPad muestra la pantalla principal del PC mientras se inicia la segunda pantalla y la pantalla del PC ya no parpadea en bucle. Actualiza el iPad a TestFlight v0.3.3, build 364, para usarlo con Windows 0.3.3.

Los diagnósticos de Windows y iPad incluyen detalles del dispositivo, errores de la aplicación y estadísticas de transmisión. Los informes incluyen nombres de tarjetas gráficas y pantallas y direcciones de red, pero excluyen el contenido de la pantalla, el audio y las credenciales de vinculación. A partir de la versión 0.3.4, los envíos de Windows eliminan el nombre del PC, el nombre de usuario, la carpeta de usuario, el nombre del iPad y los nombres de los dispositivos de audio, y conservan solo el tipo de cada dispositivo de audio; los envíos del iPad eliminan el nombre del PC. Desactiva el envío automático en **Ajustes** (Settings). Los diagnósticos se conservan hasta 30 días y pueden eliminarse antes. Los diagnósticos eliminados o vencidos pueden permanecer en copias de seguridad diarias hasta 7 días más. Consulta [Diagnósticos automáticos](../SETUP.md#automatic-diagnostics) y el [aviso de privacidad](https://eternalmonitor.dev/privacy.html). En el iPad, abre **Ajustes** (Settings) > **Soporte** (Support) > **Diagnósticos** (Diagnostics) para copiar o exportar un informe.

Las comprobaciones de la versión se aprobaron en Windows, macOS y simuladores de iPad. Los problemas conocidos están en la página de [problemas conocidos](https://eternalmonitor.dev/known-issues.html), y los resultados anteriores en dispositivos físicos siguen en [docs/hardware-verification.md](../docs/hardware-verification.md).

Limitaciones conocidas:

- La transmisión no está cifrada. La vinculación impide el acceso de otros dispositivos, así que usa una red de confianza, como tu Wi-Fi doméstica.
- La aplicación de Windows es el único host. El host se compila en macOS para desarrollo, pero allí transmite un patrón de prueba en lugar de tu pantalla.
- Apple Pencil (USB-C) no tiene sensor de presión, por lo que sus trazos mantienen un solo grosor.

[Problemas conocidos](https://eternalmonitor.dev/known-issues.html) enumera todos los problemas que conocemos y qué hacer en cada caso.

## Cómo funciona

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../docs/assets/readme/how-it-works-dark.svg">
  <img src="../docs/assets/readme/how-it-works-light.svg" width="1120" alt="El PC con Windows captura el escritorio, codifica el video en la GPU y lo envía por Wi-Fi o USB. El iPad recompone el video, solicita los fragmentos que faltan por Wi-Fi, lo decodifica con VideoToolbox y lo muestra con Metal. El audio del PC se captura con WASAPI, se codifica como Opus y se reproduce en el iPad. Las acciones táctiles, de Pencil, teclado y trackpad vuelven al PC, que las inyecta.">
</picture>

El host de Windows está escrito en Rust. Captura el escritorio con DXGI, codifica en la GPU mediante FFmpeg y envía cada fotograma como fragmentos UDP numerados. Por USB, los mismos mensajes viajan por un túnel con enmarcado proporcionado por el servicio de dispositivos de Apple. La aplicación del iPad es nativa de Swift. Recompone los fragmentos, decodifica con VideoToolbox y renderiza con Metal. En Wi-Fi también solicita los fragmentos que faltan. El audio del PC se captura con WASAPI y se envía como Opus, y el iPad oculta el audio perdido en lugar de solicitarlo de nuevo. Las acciones y los informes del receptor vuelven por la misma sesión, y el host usa esos informes para adaptar la tasa de bits.

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
| `ETERNAL_INPUT` | Entrada del codificador: `yuv420` (predeterminada), `auto` o `bgra` |
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

Estos documentos están en inglés.

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
| [docs/beta-testing.md](../docs/beta-testing.md) | Cómo ejecutar una beta: TestFlight, firma y qué probar |
| [docs/design.md](../docs/design.md) | La apariencia compartida de las aplicaciones y el sitio web |
| [RELEASE_NOTES.md](../RELEASE_NOTES.md) | Cambios de cada versión |

## Contribuir

Se aceptan contribuciones, desde transporte y codificadores hasta renderizado y documentación. Lee [CONTRIBUTING.md](../CONTRIBUTING.md) antes de abrir un pull request. Para conversar primero sobre una idea, pregunta en el [Discord de Sideband](https://discord.gg/Xc7NykBZ6z).

## Contacto

El [Discord de Sideband](https://discord.gg/Xc7NykBZ6z) es el lugar para preguntas, ayuda con la configuración e ideas. También puedes escribir a [hello@sideband.studio](mailto:hello@sideband.studio).

¿Encontraste un error? [Abre un issue](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose) con las versiones de ambas aplicaciones y el registro del host. Informa los problemas de seguridad en privado como se describe en [SECURITY.md](../SECURITY.md), no en Discord ni en un issue.

## Licencia

EternalMonitor se publica bajo la [MIT License](../LICENSE), © 2026 Sideband Studio. El instalador de Windows también incluye componentes de terceros bajo sus propias licencias, entre ellos [Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver) y FFmpeg.

Creado por [Sideband Studio](https://sideband.studio).

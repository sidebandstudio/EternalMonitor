<div align="center">

[English](../README.md) · [Español](README.es.md) · [العربية](README.ar.md) · [简体中文](README.zh-Hans.md) · [हिन्दी](README.hi.md) · [Русский](README.ru.md) · [日本語](README.ja.md) · **Português (Brasil)** · [Français](README.fr.md) · [Deutsch](README.de.md) · [한국어](README.ko.md)

*Se esta página e o [README em inglês](../README.md) forem diferentes, a versão em inglês é a mais atual.*

<img src="../docs/assets/icon.png" width="88" height="88" alt="Logo do EternalMonitor">

# EternalMonitor

Um segundo monitor gratuito e de código aberto para Windows.<br>
Conecte seu iPad por Wi-Fi ou USB, com suporte a toque e Apple Pencil.

[![Release](https://img.shields.io/github/v/release/sidebandstudio/EternalMonitor?label=release&labelColor=111&color=e8ff47)](https://github.com/sidebandstudio/EternalMonitor/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/sidebandstudio/EternalMonitor/ci.yml?branch=main&label=CI&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-e8ff47?labelColor=111)](../LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20iPadOS-e8ff47?labelColor=111)](#download)
[![Website](https://img.shields.io/badge/website-eternalmonitor.dev-e8ff47?labelColor=111)](https://eternalmonitor.dev)
[![Discord](https://img.shields.io/badge/discord-Sideband-e8ff47?labelColor=111&logo=discord&logoColor=white)](https://discord.gg/Xc7NykBZ6z)

[Baixar](#download) · [Guia de configuração](../SETUP.md) · [Site](https://eternalmonitor.dev) · [Plano de desenvolvimento](../ROADMAP.md) · [Notas da versão](../RELEASE_NOTES.md) · [Discord](https://discord.gg/Xc7NykBZ6z)

</div>

<p align="center">
  <img src="../docs/assets/readme-demo.webp" width="860" alt="Um PC com Windows e um iPad. No PC, a janela do EternalMonitor muda o iPad para Segunda tela, e uma janela é arrastada do monitor para o iPad. Em seguida, um Apple Pencil desenha folhas de grama em uma pintura no iPad, com traços grossos ao pressionar e finos ao levantar.">
</p>

O EternalMonitor transforma um iPad em monitor para seu PC com Windows. É gratuito, tem código aberto sob a licença MIT e não exige conta. O PC transmite a até 120 fps, com codificação por hardware em placas gráficas compatíveis e decodificação por hardware no iPad. Cada toque, tecla pressionada e traço do Pencil volta ao PC.

<a id="download"></a>

## Início rápido

[![Baixar para Windows](https://img.shields.io/badge/Download_for_Windows-v0.3.3-e8ff47?style=for-the-badge&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/releases/download/v0.3.3/EternalMonitor-Setup.exe)
[![Obter o aplicativo para iPad no TestFlight](https://img.shields.io/badge/iPad_app-TestFlight-e8ff47?style=for-the-badge&labelColor=111)](https://testflight.apple.com/join/ppc9QfXc)

1. **Instale o aplicativo para Windows.** Execute [EternalMonitor-Setup.exe](https://github.com/sidebandstudio/EternalMonitor/releases/download/v0.3.3/EternalMonitor-Setup.exe) no Windows 10 (versão 1809 ou posterior) ou Windows 11, 64 bits, com gráficos NVIDIA, AMD ou Intel. Se o Windows SmartScreen perguntar, escolha **Mais informações** e depois **Executar assim mesmo**. A [página da versão](https://github.com/sidebandstudio/EternalMonitor/releases/tag/v0.3.3) informa o SHA-256 do arquivo.
2. **Instale o aplicativo para iPad.** Em um iPad com iPadOS 17 ou posterior, instale o **TestFlight** pela App Store. Depois, abra o [link do EternalMonitor no TestFlight](https://testflight.apple.com/join/ppc9QfXc) no iPad e toque em **Instalar**.
3. **Conecte.** Com os dois aplicativos abertos na mesma rede, toque no seu PC no iPad ou escaneie o código QR mostrado pelo PC. Para usar USB no Windows 10 22H2 ou Windows 11, conecte o cabo e mantenha aberto no PC o aplicativo gratuito [Apple Devices](https://apps.microsoft.com/detail/9np83lwlpz9k), da Apple.

O [guia de configuração](../SETUP.md) explica cada etapa com os nomes exatos dos botões. A [página de download](https://eternalmonitor.dev/download.html) lista as versões de Windows e iPad que funcionam juntas.

Se o EternalMonitor for útil para você, uma estrela ajuda outras pessoas a encontrá-lo.

## Recursos

<img src="../docs/assets/ipad-app.webp" align="right" width="270" alt="Tela de conexão do aplicativo do iPad com Conectar, Escanear código QR e Procurar PCs">

- **Estenda ou espelhe.** Adicione o iPad como uma segunda tela ao lado do monitor ou espelhe qualquer tela. A segunda tela permanece por 10 segundos após a desconexão do iPad, então uma reconexão rápida a reutiliza e o Windows não mantém um monitor fantasma.
- **Wi-Fi ou USB.** Transmita pela rede doméstica ou conecte um cabo para obter uma imagem mais estável, que também pode carregar o iPad por uma porta USB-C. O cabo assume uma sessão Wi-Fi em andamento e, ao desconectá-lo, a transmissão volta ao Wi-Fi.
- **Apple Pencil como uma caneta de verdade.** O Windows recebe pressão e inclinação pelo Windows Ink, e a caneta marca sob a ponta em qualquer disposição de monitores. O **Modo de desenho** (Drawing mode) ignora a palma da mão na tela de desenho, e o Clip Studio Paint funciona com a opção Tablet PC.
- **Toque, teclado e ponteiro.** Toque para clicar, arraste, role com dois dedos e mantenha pressionado para clicar com o botão direito. Teclados físicos, trackpads e mouses também funcionam, com modificadores persistentes e clique do botão do meio.
- **Codificação por hardware.** Gráficos NVIDIA, AMD e Intel compatíveis codificam a transmissão a 30, 60, 90 ou 120 fps.
- **Recuperação de perdas no Wi-Fi.** Quando um pacote se perde, o iPad o solicita novamente na hora, então a perda de um pacote raramente custa um quadro. A taxa de bits acompanha sua rede.
- **Áudio do PC.** O som é reproduzido no iPad e você pode silenciá-lo sem interromper o vídeo.
- **Ajustar ou preencher.** **Ajustar** (Fit) mostra toda a imagem do PC, com barras onde os formatos são diferentes. **Preencher** (Fill) usa toda a tela do iPad e corta as bordas que não cabem.
- **Pareamento.** A primeira conexão por Wi-Fi pede um código de seis dígitos ou a leitura de um QR. Depois disso, o iPad lembra do PC. O USB não precisa de código.
- **Recupera e explica.** A imagem volta sozinha após a tela de bloqueio, uma alteração de tela ou uma reinicialização da transmissão. Os dois aplicativos informam o que aconteceu e o que fazer, e **Enviar feedback** (Send feedback) prepara um email para o estúdio com os dados da sua configuração preenchidos.
- **Atualizações com um clique.** Uma janela avisa quando há uma nova versão. No Windows, **Atualizar agora** (Update now) verifica o download e instala com uma única confirmação do Windows; no iPad, abre o TestFlight. O aplicativo do Windows funciona sem janela de console.

Este README e o [site](https://eternalmonitor.dev) estão disponíveis em 11 idiomas.

<br clear="right">

## Saiba mais

- [Use um iPad como segundo monitor para Windows](https://eternalmonitor.dev/ipad-second-monitor-windows.html): o guia completo.
- [Compatibilidade](https://eternalmonitor.dev/compatibility.html): PCs e iPads em que as pessoas usam o EternalMonitor.
- [Alternativas](https://eternalmonitor.dev/alternatives.html): como o EternalMonitor se compara a outras formas de usar um iPad como tela.
- [Plano de desenvolvimento](../ROADMAP.md): o que vem a seguir.

**Compartilhe sua configuração.** Conte quais PC e iPad você usa pelo [relatório de compatibilidade](https://github.com/sidebandstudio/EternalMonitor/issues/new?template=compatibility.yml) ou publique no [Discord Sideband](https://discord.gg/Xc7NykBZ6z). Com sua permissão, ela aparece na página de compatibilidade.

## Status

A versão atual é v0.3.3 para Windows e v0.3.3 (build 364) para iPad. Ela leva **Segunda tela** (Second screen) a mais notebooks com gráficos Intel e NVIDIA: o iPad mostra a tela principal do PC imediatamente e muda para a segunda tela quando o Windows a adiciona. [RELEASE_NOTES.md](../RELEASE_NOTES.md) descreve cada versão, e [ROADMAP.md](../ROADMAP.md), o que vem a seguir.

As verificações da versão v0.3.3 passaram no Windows, macOS e simuladores de iPad. [docs/hardware-verification.md](../docs/hardware-verification.md) registra os resultados práticos em um PC e um iPad reais desde a v0.3.0.

Os dois aplicativos compartilham diagnósticos para encontrar e corrigir problemas: detalhes do dispositivo, erros do aplicativo e estatísticas da transmissão, incluindo nomes do PC e do iPad, nomes de dispositivos de áudio e endereços de rede. Os diagnósticos nunca incluem conteúdo da tela, áudio ou credenciais de pareamento. Desative o compartilhamento em Configurações. [Diagnósticos automáticos](../SETUP.md#automatic-diagnostics) e o [aviso de privacidade](https://eternalmonitor.dev/privacy.html) detalham o que é enviado e por quanto tempo é mantido. No iPad, abra Configurações > Suporte > Diagnósticos para copiar ou exportar um relatório.

## Como funciona

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../docs/assets/readme/how-it-works-dark.svg">
  <img src="../docs/assets/readme/how-it-works-light.svg" width="1120" alt="O PC com Windows captura a área de trabalho, codifica o vídeo na GPU e o envia por Wi-Fi ou USB. O iPad remonta o vídeo, solicita fragmentos ausentes no Wi-Fi, decodifica com VideoToolbox e exibe com Metal. O áudio do PC é capturado com WASAPI, codificado como Opus e reproduzido no iPad. As entradas de toque, Pencil, teclado e trackpad retornam ao PC, que as injeta.">
</picture>

O host Windows é escrito em Rust. Ele captura a área de trabalho com DXGI, codifica por meio do FFmpeg com NVIDIA NVENC, AMD AMF ou Intel Quick Sync em hardware gráfico compatível (H.264 por padrão, ou HEVC) e envia cada quadro como fragmentos UDP numerados. Por USB, as mesmas mensagens trafegam por um túnel com enquadramento fornecido pelo serviço de dispositivos da Apple. O aplicativo do iPad é nativo em Swift. Ele remonta os fragmentos, decodifica com VideoToolbox e renderiza com Metal. No Wi-Fi, também solicita fragmentos ausentes. O áudio do PC é capturado com WASAPI e enviado como Opus, e o iPad o reproduz por um buffer de jitter e compensa o áudio perdido em vez de solicitá-lo novamente. As entradas e os relatórios do receptor voltam pela mesma sessão, e o host usa esses relatórios para adaptar a taxa de bits entre 4 e 50 Mbps.

[ARCHITECTURE.md](../ARCHITECTURE.md) descreve o fluxo e o protocolo v2, e [docs/decisions.md](../docs/decisions.md) explica por que cada parte funciona dessa forma.

## Compilar a partir do código-fonte

O workspace contém o host Rust (`host/`), o crate de protocolo `eternal-wire` (`proto/`) e o aplicativo Swift para iPad (`ios/`).

<details>
<summary><b>Host Windows</b></summary>

Requisitos: Windows 10 versão 1809 ou posterior, Rust 1.98.0 (fixado por `rust-toolchain.toml`, MSVC), um SDK FFmpeg **7.1 compartilhado** e LLVM/libclang para bindgen.

```powershell
# Point the build at your FFmpeg 7.1 shared SDK (the folder with bin\avcodec-*.dll)
$env:FFMPEG_DIR = "C:\ffmpeg"
cargo build --release -p eternal-host
.\target\release\eternal-host.exe          # optional port argument, default 9876
```

O host é um programa gráfico do Windows, então não abre uma janela de console. Iniciado por um terminal, ele imprime o log ali, mas o prompt retorna imediatamente; `cargo run -p eternal-host` espera seu encerramento. Com `ETERNAL_HEADLESS=1` e a saída redirecionada (`> host.log`), ele recebe seu próprio console, então Ctrl+C no terminal não o alcança: pressione Ctrl+C na janela desse console ou use o Gerenciador de Tarefas. `scripts\win\Stop-Host.ps1` interrompe somente hosts iniciados por `remote.sh run-host`.

`scripts\build-installer.ps1` compila o Setup.exe completo com Inno Setup e o mesmo `FFMPEG_DIR`. `scripts\package.ps1` cria um zip básico.

</details>

<details>
<summary><b>Ciclo de desenvolvimento no macOS</b> (sem precisar de Windows)</summary>

No macOS, o host substitui a captura de tela por um padrão de teste sintético. O protocolo, o codificador, o transporte e o supervisor funcionam de verdade.

```bash
brew install ffmpeg@7 pkgconf xcodegen
export PKG_CONFIG_PATH=/opt/homebrew/opt/ffmpeg@7/lib/pkgconfig
cargo test --workspace          # unit, golden-vector and synthetic end-to-end tests
ETERNAL_CAPTURE=synthetic cargo run -p eternal-host
```

Se as ferramentas de linha de comando do Xcode forem o diretório de desenvolvedor selecionado, prefixe os comandos do Xcode com `DEVELOPER_DIR=/Applications/Xcode.app/Contents/Developer`.

</details>

<details>
<summary><b>Aplicativo para iPad</b></summary>

```bash
cd ios
xcodegen generate               # project.yml is the source of truth
xcodebuild test -project EternalMonitor.xcodeproj -scheme EternalMonitor \
  -destination 'platform=iOS Simulator,name=iPad Pro 11-inch (M4)'
```

Para executar em um iPad físico, abra o projeto gerado no Xcode e selecione sua própria equipe de assinatura.

</details>

<details>
<summary><b>Testes de ponta a ponta</b></summary>

```bash
./scripts/e2e_ios.sh              # synthetic host to the iPad simulator, H.264
EM_CODEC=hevc ./scripts/e2e_ios.sh
scripts/test_ios.sh               # iPad unit and UI tests
scripts/e2e_matrix.sh             # UDP, loss, burst, USB, audio, pairing, input, lifecycle
scripts/soak.sh 1800              # 30-minute frame-rate and memory check
```

Cada linha verifica a taxa de quadros decodificados (pelo menos 55 fps), perdas, recuperações e pixels renderizados. `e2e_matrix.sh --real` executa as linhas de hardware Windows pelo executor em `scripts/win/`; leia [scripts/win/README.md](../scripts/win/README.md) primeiro. O CI executa as suítes Rust no Linux, macOS e Windows, os testes do iPad e um teste de sistema de transmissão em cada pull request.

</details>

<details>
<summary><b>Variáveis de ambiente do host</b></summary>

| Variável | Efeito |
| --- | --- |
| `ETERNAL_ENCODER` | Força um codificador (`h264_nvenc`, `h264_amf`, `h264_qsv`, `libx264`) |
| `ETERNAL_HEVC` | `1` ou `0` substitui a preferência por HEVC |
| `ETERNAL_FPS` | Substitui a taxa de quadros do host; a preferência do iPad ainda define o limite |
| `ETERNAL_INPUT` | Entrada do codificador: `automatic` (padrão: BGRA para NVIDIA, YUV420 para os demais), `yuv420`, `auto` ou `bgra` |
| `ETERNAL_MAX_DGRAM` | Tamanho do datagrama de mídia, de 576 a 1400 bytes |
| `ETERNAL_CAPTURE` | `synthetic` substitui DXGI por um padrão de teste gerado |
| `ETERNAL_AUDIO` | `synthetic` seleciona a fonte de teste de áudio determinística |
| `ETERNAL_HEADLESS` | `1` executa sem janela; Ctrl+C encerra corretamente |
| `ETERNAL_ABR` | `0` desativa a taxa de bits adaptativa |
| `ETERNAL_VDD_TIMEOUT_SECS` | Tempo de espera para conectar a tela virtual (padrão 30) |
| `ETERNAL_AMF_DIAG` | `1` grava diagnósticos do fluxo de bits AMF |
| `ETERNAL_LEGACY_PTS` | `1` restaura os carimbos de tempo antigos baseados no contador de quadros |
| `ETERNAL_DROP`, `ETERNAL_REORDER`, `ETERNAL_JITTER_MS` | Somente teste: injeta perda, reordenação ou jitter |
| `ETERNAL_USB_DIRECT` | Somente teste: um endpoint TCP no lugar do túnel USB |

</details>

## Documentação

| Documento | Conteúdo |
| --- | --- |
| [SETUP.md](../SETUP.md) | Configuração passo a passo para qualquer pessoa, incluindo USB e desenho |
| [Problemas conhecidos](https://eternalmonitor.dev/known-issues.html) | Problemas conhecidos e o que fazer em cada caso (`docs/known-issues.html`) |
| [Privacidade](https://eternalmonitor.dev/privacy.html) | O que os aplicativos enviam, por quê, por quanto tempo é mantido e como desativar (`docs/privacy.html`) |
| [ARCHITECTURE.md](../ARCHITECTURE.md) | O fluxo, o protocolo v2 e cada componente |
| [docs/decisions.md](../docs/decisions.md) | Por que as coisas são assim, com medições |
| [docs/pencil-drawing.md](../docs/pencil-drawing.md) | Apple Pencil, Windows Ink e Clip Studio Paint |
| [docs/pairing.md](../docs/pairing.md), [docs/keyboard-input.md](../docs/keyboard-input.md), [docs/host-settings.md](../docs/host-settings.md), [docs/audio-codec.md](../docs/audio-codec.md) | Detalhes dos recursos |
| [docs/hardware-verification.md](../docs/hardware-verification.md) | Registro de testes da release e lista de verificação prática |
| [Testes de release](../docs/beta-testing.md) | Publicação de uma build pelo TestFlight: assinatura e o que testar |
| [docs/design.md](../docs/design.md) | A aparência compartilhada dos aplicativos e do site |
| [RELEASE_NOTES.md](../RELEASE_NOTES.md) | O que mudou em cada versão |
| [ROADMAP.md](../ROADMAP.md) | O que vem a seguir |

## Contribuindo

Todo tipo de contribuição é bem-vindo: issues, ideias, correções, documentação, traduções e novos recursos. Pull requests são bem-vindos, grandes ou pequenos. [CONTRIBUTING.md](../CONTRIBUTING.md) explica como enviar um e lista boas primeiras contribuições, e [ROADMAP.md](../ROADMAP.md) mostra o trabalho maior pela frente. Dúvidas e ideias são bem-vindas no [Discord Sideband](https://discord.gg/Xc7NykBZ6z).

## Contato

O [Discord Sideband](https://discord.gg/Xc7NykBZ6z) é o espaço para dúvidas, ajuda com a configuração e ideias. Você também pode enviar um email para [hello@sideband.studio](mailto:hello@sideband.studio).

Encontrou um erro? [Abra uma issue](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose) com as versões dos dois aplicativos e o log do host ou use o [formulário de feedback na página de download](https://eternalmonitor.dev/download.html#feedback), que envia um email ao estúdio. Relate problemas de segurança em particular, conforme descrito em [SECURITY.md](../SECURITY.md), e não no Discord ou em uma issue.

## Licença

O EternalMonitor é distribuído sob a [MIT License](../LICENSE), © 2026 Sideband Studio. O instalador para Windows também inclui componentes de terceiros sob suas próprias licenças, incluindo o [Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver) e o FFmpeg.

Criado por [Sideband Studio](https://sideband.studio).

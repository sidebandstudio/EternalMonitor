<div align="center">

[English](../README.md) · [Español](README.es.md) · [العربية](README.ar.md) · [简体中文](README.zh-Hans.md) · [हिन्दी](README.hi.md) · [Русский](README.ru.md) · [日本語](README.ja.md) · **Português (Brasil)** · [Français](README.fr.md) · [Deutsch](README.de.md) · [한국어](README.ko.md)

*Se esta página e o [README em inglês](../README.md) forem diferentes, a versão em inglês é a mais atual.*

<img src="../docs/assets/icon.png" width="88" height="88" alt="Logo do EternalMonitor">

# EternalMonitor

Espelhe ou estenda a área de trabalho do Windows em um iPad por Wi-Fi ou USB,<br>
e controle o PC por toque, teclado ou Apple Pencil.

[![Release](https://img.shields.io/github/v/release/sidebandstudio/EternalMonitor?label=release&labelColor=111&color=e8ff47)](https://github.com/sidebandstudio/EternalMonitor/releases/latest)
[![CI](https://img.shields.io/github/actions/workflow/status/sidebandstudio/EternalMonitor/ci.yml?branch=main&label=CI&labelColor=111)](https://github.com/sidebandstudio/EternalMonitor/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-e8ff47?labelColor=111)](../LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-Windows%20%7C%20iPadOS-e8ff47?labelColor=111)](#baixar)
[![Website](https://img.shields.io/badge/website-eternalmonitor.dev-e8ff47?labelColor=111)](https://eternalmonitor.dev)
[![Discord](https://img.shields.io/badge/discord-Sideband-e8ff47?labelColor=111&logo=discord&logoColor=white)](https://discord.gg/Xc7NykBZ6z)

[Baixar](#baixar) · [Guia de configuração](../SETUP.md) · [Problemas conhecidos](https://eternalmonitor.dev/known-issues.html) · [Site](https://eternalmonitor.dev) · [Notas da versão](../RELEASE_NOTES.md) · [Discord](https://discord.gg/Xc7NykBZ6z)

</div>

<p align="center">
  <img src="../docs/assets/og.png" width="860" alt="O aplicativo EternalMonitor para Windows transmite para um iPad que mostra a imagem do PC">
</p>

O EternalMonitor transmite a área de trabalho do Windows para um iPad e envia suas ações de volta ao PC. Um pequeno aplicativo para Windows captura a tela e a codifica na placa gráfica. O aplicativo do iPad decodifica o vídeo por hardware e o desenha com Metal; cada toque, tecla pressionada e traço do Pencil volta ao PC. Ele é gratuito, tem código aberto sob a MIT License e não exige conta.

## Recursos

<img src="../docs/assets/ipad-app.webp" align="right" width="270" alt="Tela de conexão do aplicativo do iPad com Conectar, Escanear código QR e Procurar PCs">

- **Estenda ou espelhe.** Adicione o iPad como uma tela extra ao lado do monitor ou espelhe qualquer tela. A tela extra desaparece 10 segundos após a desconexão do iPad, então uma reconexão rápida a reutiliza e o Windows não mantém um monitor fantasma.
- **Wi-Fi ou USB.** Transmita pela rede doméstica ou conecte um cabo para obter uma imagem mais estável e carregar o iPad. O cabo assume uma sessão Wi-Fi em andamento e, ao desconectá-lo, a transmissão volta ao Wi-Fi.
- **Toque, teclado e ponteiro.** Toque para clicar, arraste, role com dois dedos e mantenha pressionado para clicar com o botão direito. Teclados físicos, trackpads e mouses também funcionam, com modificadores persistentes e clique do botão do meio.
- **Apple Pencil como uma caneta de verdade.** O Windows recebe pressão e inclinação pelo Windows Ink, e a caneta marca sob a ponta em qualquer disposição de monitores. O **Modo de desenho** (Drawing mode) ignora a palma da mão na tela de desenho, e o Clip Studio Paint funciona com a opção Tablet PC.
- **Ajustar ou preencher.** **Ajustar** (Fit) mostra toda a imagem do PC, com barras onde os formatos são diferentes. **Preencher** (Fill) usa toda a tela do iPad e corta as bordas que não cabem.
- **Áudio do PC.** O som é reproduzido no iPad como Opus, com buffer de jitter e compensação de perdas. Você pode silenciá-lo sem interromper o vídeo.
- **Recuperação de perdas no Wi-Fi.** Quando um pacote se perde, o iPad solicita o fragmento novamente na hora, então a perda de um pacote raramente custa um quadro. A taxa de bits se adapta entre 4 e 50 Mbps.
- **Sua GPU faz o trabalho.** NVIDIA NVENC, AMD AMF ou Intel Quick Sync codificam em H.264 por padrão, ou em HEVC se você preferir, a 30, 60, 90 ou 120 fps.
- **Pareamento.** A primeira conexão por Wi-Fi pede um código de seis dígitos ou a leitura de um QR. Depois disso, o iPad lembra do PC. O USB não precisa de código.
- **Recupera e explica.** A imagem volta sozinha após a tela de bloqueio, uma alteração de tela ou uma reinicialização da transmissão. Quando algo dá errado, os dois aplicativos informam o que aconteceu e o que fazer, e **Enviar feedback** (Send feedback) prepara um email para o estúdio com os dados da sua configuração preenchidos.
- **Atualizações com um clique.** Uma janela avisa quando há uma nova versão. No Windows, **Atualizar agora** (Update now) verifica o download e instala com uma única confirmação do Windows; o aplicativo do iPad abre o TestFlight. O aplicativo do Windows funciona sem janela de console.

<br clear="right">

## Baixar

| | Requisitos | Obtenha |
| --- | --- | --- |
| **Windows** | Windows 10 versão 1809 ou posterior, ou Windows 11, 64 bits, com gráficos NVIDIA, AMD ou Intel | [EternalMonitor-Setup.exe](https://github.com/sidebandstudio/EternalMonitor/releases/tag/v0.3.3) (v0.3.3 estável atual) |
| **iPad** | iPadOS 17 ou posterior | [TestFlight v0.3.3, build 364](https://testflight.apple.com/join/ppc9QfXc). A versão beta pública está aberta: instale o TestFlight e abra o link no iPad. |

Instale as versões para Windows e iPad indicadas em eternalmonitor.dev/download.html. Elas funcionam juntas mesmo quando os números de versão são diferentes. As versões antigas v0.1 usam outro protocolo e não conseguem se conectar ao aplicativo v0.3 do iPad.

Está testando? Envie erros e comentários pelo [formulário na página de download](https://eternalmonitor.dev/download.html#feedback), que envia um email para hello@sideband.studio, abra uma [issue no GitHub](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose) ou fale conosco no [Discord Sideband](https://discord.gg/Xc7NykBZ6z).

O instalador ainda não tem assinatura de código, então o Windows SmartScreen pede confirmação antes de executá-lo. Escolha **Mais informações** (More info) e depois **Executar assim mesmo** (Run anyway), somente após conferir se o SHA-256 do arquivo corresponde ao da [página de releases](https://github.com/sidebandstudio/EternalMonitor/releases).

Para usar USB, você também precisa do aplicativo gratuito [Apple Devices](https://apps.microsoft.com/detail/9np83lwlpz9k), da Apple, aberto durante a transmissão. O Windows só consegue acessar um iPad por cabo através dele, e o EternalMonitor avisa quando ele está fechado ou não está instalado. O Apple Devices exige Windows 10 versão 22H2 (build 19045) ou Windows 11. Em versões anteriores do Windows 10, conecte por Wi-Fi.

**Em seguida, siga o [guia de configuração](../SETUP.md).** Ele explica a instalação dos dois aplicativos, o pareamento, o USB, a tela estendida e o desenho, com os nomes exatos dos botões que você verá.

## Status

A v0.3.3 corrige **Segunda tela** (Second screen) nos PCs em que ela não iniciava, principalmente notebooks com gráficos Intel e NVIDIA. O iPad mostra a tela principal do PC enquanto a segunda tela inicia, e a tela do PC não pisca mais repetidamente. Atualize o iPad para TestFlight v0.3.3, build 364, para usá-lo com Windows 0.3.3.

Os diagnósticos do Windows e do iPad incluem detalhes do dispositivo, erros do aplicativo e estatísticas da transmissão. Os relatórios incluem nomes do PC e do iPad, nomes de dispositivos de áudio e endereços de rede, mas não incluem conteúdo da tela, áudio nem credenciais de pareamento. Desative o compartilhamento automático em **Configurações** (Settings). Os diagnósticos são mantidos por até 30 dias e podem ser removidos antes. Diagnósticos excluídos ou expirados podem permanecer nos backups diários por até mais 7 dias. Consulte [Diagnósticos automáticos](../SETUP.md#automatic-diagnostics) e o [aviso de privacidade](https://eternalmonitor.dev/privacy.html). No iPad, abra **Configurações** (Settings) > **Suporte** (Support) > **Diagnósticos** (Diagnostics) para copiar ou exportar um relatório.

As verificações de release passaram no Windows, macOS e simuladores de iPad. Os problemas conhecidos estão na página [Problemas conhecidos](https://eternalmonitor.dev/known-issues.html), e os resultados anteriores em dispositivos físicos permanecem em [docs/hardware-verification.md](../docs/hardware-verification.md).

Limitações conhecidas:

- A transmissão não é criptografada. O pareamento impede o acesso de outros dispositivos, então use uma rede de confiança, como seu Wi-Fi doméstico.
- O aplicativo do Windows é o único host. O host compila no macOS para desenvolvimento, mas nele transmite um padrão de teste em vez da tela.
- O Apple Pencil (USB-C) não tem sensor de pressão, então seus traços mantêm a mesma espessura.

[Problemas conhecidos](https://eternalmonitor.dev/known-issues.html) lista todos os problemas conhecidos e o que fazer em cada caso.

## Como funciona

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../docs/assets/readme/how-it-works-dark.svg">
  <img src="../docs/assets/readme/how-it-works-light.svg" width="1120" alt="O PC com Windows captura a área de trabalho, codifica o vídeo na GPU e o envia por Wi-Fi ou USB. O iPad remonta o vídeo, solicita fragmentos ausentes no Wi-Fi, decodifica com VideoToolbox e exibe com Metal. O áudio do PC é capturado com WASAPI, codificado como Opus e reproduzido no iPad. As entradas de toque, Pencil, teclado e trackpad retornam ao PC, que as injeta.">
</picture>

O host Windows é escrito em Rust. Ele captura a área de trabalho com DXGI, codifica na GPU por meio do FFmpeg e envia cada quadro como fragmentos UDP numerados. Por USB, as mesmas mensagens trafegam por um túnel com enquadramento fornecido pelo serviço de dispositivos da Apple. O aplicativo do iPad é nativo em Swift. Ele remonta os fragmentos, decodifica com VideoToolbox e renderiza com Metal. No Wi-Fi, também solicita fragmentos ausentes. O áudio do PC é capturado com WASAPI e enviado como Opus, e o iPad compensa o áudio perdido em vez de solicitá-lo novamente. As entradas e os relatórios do receptor voltam pela mesma sessão, e o host usa esses relatórios para adaptar a taxa de bits.

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
| `ETERNAL_INPUT` | Entrada do codificador: `yuv420` (padrão), `auto` ou `bgra` |
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

Estes documentos estão em inglês.

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
| [docs/beta-testing.md](../docs/beta-testing.md) | Como executar uma versão beta: TestFlight, assinatura e o que testar |
| [docs/design.md](../docs/design.md) | A aparência compartilhada dos aplicativos e do site |
| [RELEASE_NOTES.md](../RELEASE_NOTES.md) | O que mudou em cada versão |

## Contribuindo

Contribuições são bem-vindas em transporte, codificadores, renderização e documentação. Leia [CONTRIBUTING.md](../CONTRIBUTING.md) antes de abrir um pull request. Para discutir uma ideia primeiro, pergunte no [Discord Sideband](https://discord.gg/Xc7NykBZ6z).

## Contato

O [Discord Sideband](https://discord.gg/Xc7NykBZ6z) é o espaço para dúvidas, ajuda com a configuração e ideias. Você também pode enviar um email para [hello@sideband.studio](mailto:hello@sideband.studio).

Encontrou um erro? [Abra uma issue](https://github.com/sidebandstudio/EternalMonitor/issues/new/choose) com as versões dos dois aplicativos e o log do host. Relate problemas de segurança em particular, conforme descrito em [SECURITY.md](../SECURITY.md), e não no Discord ou em uma issue.

## Licença

O EternalMonitor é distribuído sob a [MIT License](../LICENSE), © 2026 Sideband Studio. O instalador para Windows também inclui componentes de terceiros sob suas próprias licenças, incluindo o [Virtual Display Driver](https://github.com/VirtualDrivers/Virtual-Display-Driver) e o FFmpeg.

Criado por [Sideband Studio](https://sideband.studio).

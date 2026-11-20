# FlowScale 2

Um app para Windows 10/11 que junta quatro ferramentas:
1. **Escalador de janelas** (estilo Lossless Scaling)
2. **Geração de frames** por fluxo óptico, em qualquer janela
3. **Limitador de FPS dentro do jogo** (estilo Framepacer)
4. **FlowScale SR**: upscaler temporal que roda dentro do jogo, no lugar do FSR 3.1 ou do XeSS

Escrito do zero em Rust, com base na engenharia reversa do comportamento dos dois apps (veja `../LosslessScaling-REA.md` e `../Framepacer-REA.md`). Nenhum shader, código ou asset deles foi reaproveitado.

## Distribuição (`build.ps1` → `dist/`, ~1,8 MB)
| Arquivo | O que é |
|---|---|
| `flowscale.exe` | App: UI (WebView2 + Tailwind embutido), motor de escala/FG, detector e injetor |
| `flowscale_hook.dll` | Limitador que roda dentro do jogo (x64). Também é a layer Vulkan |
| `flowscale_sr.dll` | FlowScale SR. Uma DLL só: vira `amd_fidelityfx_dx12.dll` / `amd_fidelityfx_loader_dx12.dll` (FSR 3.1) ou `libxess.dll` (XeSS) no jogo |
| `licenses/` | Licenças MIT/BSD do código de terceiros (FlowScale SR, NIS, Snapdragon GSR, Anime4K, xBR, FidelityFX CAS) |
| `flowscale32.exe`, `flowscale_hook32.dll` | Suporte a jogos 32 bits |
| `WebView2Loader.dll` | Loader do WebView2 (o runtime já vem no Windows 11) |

## Recursos
**Escala (13 escaladores):** Nearest, Integer, Bilinear, Sharp bilinear, Bicubic, Lanczos 3, **FSR 1** (Qualidade ou Desempenho), Line (anime), **NVIDIA Image Scaling** (shader oficial, MIT), **Snapdragon GSR** (porte do oficial, BSD-3), **xBR** nível 2 (pixel art, MIT) e **Anime4K** v3.2 nas redes S, M, L, VL e UL (MIT, convertidas por `tools/anime4k_convert.py`). A nitidez final é **RCAS** ou **CAS** (AMD FidelityFX). Há também **escala por região (VRS)**, que reescala só os blocos 16×16 que mudaram e é idêntica a um redesenho completo (teste no `-scalebench`). E ainda: fit/stretch, fator customizado, redimensionar o jogo antes, crop, HDR scRGB, escolha de GPU e monitor, e cursor redesenhado, escalado, preso na janela ou oculto.

Medido no `-scalebench` (640×360 → 1280×720, PSNR contra o original, GTX 1650): Bilinear 34,1 dB · NIS 35,1 · Bicubic 36,3 · Lanczos 37,1 · Anime4K S 35,5 / M 36,4 / L 37,4 / VL 38,1 / **UL 38,4 dB** (5,2 ms).

**Captura:** Graphics Capture com **fila configurável** (0 = sempre o quadro mais novo, 1–2 = mais estável sob carga), Desktop Duplication (com **monitores girados**) e **GDI** (PrintWindow) para janelas que as outras não leem. **Seguir a janela do jogo:** quando o jogo fecha e recria a janela (troca de modo, launcher → jogo), a escala volta sozinha. **G-SYNC** (NVIDIA): um perfil de driver só para o `flowscale.exe` libera VRR em janela. Desligar a opção desfaz.

**Geração de frames:** (com FPS mínimo para gerar e detecção de repetidos ligável)
- Fluxo óptico próprio, do nível grosso ao fino e nos dois sentidos, com subpixel, mediana e oclusão.
- Proteção de HUD.
- Modo **fixo** (x2–x8) ou **alvo de FPS** (passo fracionário).
- Respeita a taxa de atualização com VSync: nunca gera mais que o monitor mostra, para não acumular latência.
- Descarta frames gerados atrasados.
- **Detecta quadros duplicados** (emuladores e jogos travados em 30 que apresentam a 60+): só interpola entre quadros realmente novos.
- Com VSync, cada quadro é alinhado ao **vblank real** do monitor (σ 0,03 ms em 30→180 FPS).
- O intervalo real é estimado pela inclinação dos timestamps de captura, imune à quantização do DWM.
- GPU automática = a que controla o monitor de destino (notebooks na bateria não caem mais na GPU integrada).

**Limitador:**
- Hooks de D3D9, D3D10/11, D3D12, OpenGL e Vulkan; jogos x64 e x86.
- Precisão de 0,01 FPS: deadline absoluto com resto inteiro, timer de alta resolução por thread e spin adaptativo.
- Caps: valor custom, taxa de atualização, VRR (refresh − 3%), meia taxa, e **Match frame gen** (refresh ÷ multiplicador, que trava o jogo na entrada ideal do FG).
- Espera *Smooth* (antes do Present) ou *Low latency* (depois do Present).
- Detecta jogos sozinho e **não injeta quando encontra anti-cheat** (EAC, BattlEye, Vanguard, FACEIT…).

**Upscaling no jogo (FlowScale SR):**
- Reconstrução temporal a partir da profundidade, dos vetores de movimento e do jitter do próprio jogo: passes convert → motion → upscale → sharpen em compute D3D12, núcleo do DittoSR 0.5.1 (MIT, derivado do Snapdragon GSR 2) portado para Rust.
- Estabilidade por pixel e anti-cintilação, detecção de movimento sem vetores (grama, reflexos), histórico 10-bit, FP16 opcional, sharpen CAS ou RCAS fundido, auto-exposição e Relight experimental (AO + 1 rebote de GI).
- Entra pelo **FSR 3.1** (FidelityFX API) ou pelo **XeSS** (D3D12). A DLL original fica ao lado (`.orig.dll` / `libxess_orig.dll`): o frame generation do jogo continua funcionando, e F11 compara lado a lado.
- A página **Upscaling no jogo** acha os jogos (Steam, Epic, GOG, Xbox, Heroic e pastas adicionadas), instala com backup, restaura e grava os ajustes. F9 recarrega os ajustes com o jogo aberto.
- Telemetria ao vivo no app: jogo, entrada, resolução de render → saída e tempo de GPU.
- Nunca toca em jogos com anti-cheat.

**Saída sem tela cheia:** modo **Em janela** desenha só sobre a área do jogo (acompanha mover e redimensionar). A área de trabalho continua visível e o mouse funciona normal. O modo **Tela cheia** mantém a escala clássica para o monitor inteiro.

**Overlay no jogo:** painel nítido (texto suavizado, cantos arredondados, tema claro ou escuro) com FPS, quadros gerados (jogo → saída), tempo de quadro, 1% mínima, limite e gráfico de tempo de quadro. Tem 3 níveis de detalhe, 4 posições, tamanho e opacidade ajustáveis. Aparece sobre a imagem escalada e também **só com o limitador**, sobre jogos em janela ou sem borda. Fica fora de capturas e gravações.

**App:**
- Três telas: **Início** (escala, limitador, gráfico ao vivo e ajustes rápidos), **Ajustes** (tudo num lugar, com busca e índice) e **No jogo** (FlowScale SR). Tema **claro, escuro ou do Windows**; **português, inglês e espanhol**.
- Perfis por `.exe` (escala, geração e limitador juntos), que também valem para **outros exes, caminhos ou títulos de janela** (vários separados por `;`, com curingas `*` e `?`), e escala automática com atraso.
- Opção de **sempre abrir como administrador**.
- Overlay com **cor de destaque, fonte e deslocamento** ajustáveis.
- Todos os shaders são **pré-compilados no build**: a escala começa na hora, sem compilador em tempo de execução.
- Atalhos configuráveis (escalar, pausar limitador, mostrar overlay). Padrões: **Ctrl+Alt+S**, **Ctrl+Alt+L**, **Ctrl+Alt+O**.
- A injeção do limitador roda em segundo plano: a janela nunca trava.
- Mais leve: o WebView2 suspende quando o app vai para a bandeja, e o cache dele fica em `%LOCALAPPDATA%\FlowScale`. A UI não depende mais de npm.
- Se o app cair, o cursor e a velocidade do mouse voltam ao normal na próxima abertura.
- **RivaTuner**: detecta o overlay duplicado e exclui o FlowScale do RTSS com um clique.

## Medido nesta máquina (GTX 1650, monitor de 180 Hz)
**Limitador**, contra jogos rodando a 1.500–13.000 FPS:

| Alvo | Resultado | σ |
|---|---|---|
| D3D11 / D3D9 / OpenGL x64 (jogo de teste) | 60,00 / 59,94 / 144,01 | ≤ 0,008 ms |
| D3D11 / D3D9 **32 bits** | 75,00 / 75,00 | ≤ 0,011 ms |
| **Godot 4.7, engine real**: Vulkan / D3D12 / OpenGL | 72,00 / 72,00 / 72,00 | 0,000 / 0,141 / 0,016 ms |

**Geração de quadros**, benchmark sintético com ground truth (`-fgbench`, PSNR, quanto maior melhor):

| | v2.0 | agora |
|---|---|---|
| Média (mesmos 4 casos) | 32,0 dB | **35,5 dB** |
| Movimento rápido (40 px) | 19,4 dB | **29,8 dB** |
| HUD | 35–47 dB | **47–55 dB** |
| Custo em 1080p (Desempenho / Equilibrado) | 9,6 / 55 ms | **3,9 / 9,3 ms** |

**Engine real, combo completo**: Godot Vulkan limitada em 90 FPS + FG x2 + FSR 1 → **180 FPS em 180 Hz, σ 0,21 ms**, com o modo "Casar com a geração".

**FlowScale SR**, harness offline do DittoSR (cenas sintéticas, 1440×810, PSNR contra 4×4 supersample):

| Cena | DittoSR 0.5.1 | FlowScale SR |
|---|---|---|
| Tiles, 2× | 31,71 dB | 31,71 dB |
| Grama, pan irregular, 1,5× | 35,37 dB, shimmer 3,69 | igual |
| Brilhos HDR, 2× | 28,63 dB, flicker 16,43 | igual |
| Grama ao vento + reflexos + sombra, 3× | 28,46 dB | igual |
| Relight, tiles 2× | 29,55 dB | igual |

Saída idêntica à referência, ~1,5 ms em 1080p na GTX 1650 (FSR 3.1 da AMD na mesma cena: ~9,9 ms). Pelo XeSS: 49,6 dB contra a referência analítica (XeSS 2.0.2 da Intel: 50,4 dB). Instalar e restaurar deixa a pasta do jogo idêntica byte a byte.

## Build
```
powershell -ExecutionPolicy Bypass -File build.ps1
```
Requisitos: Rust (MSVC), Node (só para embutir o CSS na UI, sem `npm install`), Build Tools do VS e o Windows SDK (o `dxc.exe` dele compila os shaders do FlowScale SR).

## Linha de comando
```
flowscale.exe -minimized | -page limiter | -devtools
flowscale.exe -scale "título" [seg]          escala uma janela
flowscale.exe -limit "título" <fps> <seg>    injeta, mede e grava o resultado em flowscale.log
flowscale.exe -selftest                      valida shaders, FG e overlay offscreen
flowscale.exe -osdshot <arquivo.bmp>         renderiza o overlay em todos os estilos
flowscale.exe -scalebench [pasta]             qualidade (PSNR) e custo de cada escalador; valida o VRS
flowscale.exe -gsync status|on|off            consulta (ou liga/desliga) o perfil de G-SYNC do driver NVIDIA
flowscale.exe -fgbench                       qualidade do FG (PSNR) em cenas sintéticas
flowscale.exe -fgcost                        custo do FG na GPU em 720p/1080p
flowscale.exe -games                         lista os jogos encontrados no flowscale.log
flowscale.exe -game <pasta> install|restore  instala ou restaura o FlowScale SR num jogo
```

## Código
`src/main.rs` (app, ponte com a UI, detector, seguir janela, tema) · `src/scale.rs` (pipeline de escala: NIS, Anime4K, VRS, RCAS/CAS) · `src/gsync.rs` (perfil NVAPI) · `src/a4k_passes.rs` + `src/shaders/anime4k/` (gerados) · `src/shaders/third_party/` (fontes oficiais) · `tools/anime4k_convert.py` · `src/config.rs` (esquema único de ajustes, ini) · `src/osd.rs` (overlay no jogo) · `src/engine.rs` (captura, escala, FG, apresentação) · `src/shaders.hlsl` · `src/pacer.rs` (pacer compartilhado) · `src/hook.rs` (DLL do jogo + layer Vulkan) · `src/ipc.rs` (memória compartilhada) · `src/limiter.rs` (injeção, detecção, anti-cheat, layer) · `ui/src/` (HTML + CSS, sem dependências) · `src/ingame.rs` (jogos, instalação, backup) · `sr/` (FlowScale SR: `upscaler.rs` D3D12, `lib.rs` entrada FSR 3.1, `xess.rs` entrada XeSS, `shaders/`) · `test/` (jogo de teste D3D11/D3D9/GL e teste Vulkan).

## Limites honestos
- O FG é um fluxo óptico clássico, sem ML. Movimentos muito rápidos e transparências podem gerar artefatos.
- O caminho de Present do Vulkan foi validado na criação da cadeia e na resolução da função, mas ainda não num jogo Vulkan apresentando frames.
- Jogos elevados exigem rodar o FlowScale como administrador.
- Jogos com anti-cheat não são limitados nem modificados, de propósito.
- O FlowScale SR entra hoje por FSR 3.1 e XeSS em D3D12. A entrada DLSS, a de FSR 2 e o caminho Vulkan ainda não existem.
- O FlowScale SR foi validado no harness e em jogos de teste, não num jogo comercial: esta máquina não tem nenhum jogo instalado.

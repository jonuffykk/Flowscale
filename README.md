# FlowScale

Escala, geração de quadros, limitador de FPS e upscaling dentro do jogo, num app só para Windows 10 e 11. Escrito em Rust, leve e sem instalação.

[Baixar a última versão](https://github.com/jonuffykk/Flowscale/releases/latest) · [Novidades](CHANGELOG.md) · [Contribuir](CONTRIBUTING.md) · Licença MIT

## O que ele faz

**Escala qualquer janela.** São 13 escaladores, de Nearest e Integer para pixel art até FSR 1, NVIDIA Image Scaling, Snapdragon GSR, xBR e Anime4K (S a UL). Tudo termina com nitidez RCAS ou CAS. A escala por região redesenha só os blocos que mudaram, com resultado idêntico ao redesenho completo. Funciona em tela cheia ou só sobre a janela do jogo, sem exigir fullscreen, e acompanha o jogo quando ele recria a janela.

**Gera quadros em qualquer jogo.** O fluxo óptico é próprio, em pirâmide, nos dois sentidos, com subpixel, oclusão e proteção de HUD. Tem modo fixo (2× a 8×) ou alvo de FPS. Ele ignora quadros repetidos e não mistura duas cenas num corte. A qualidade automática mede o custo na GPU e reduz a resolução do fluxo antes de tirar FPS do jogo. A saída nunca passa do monitor, e com G-SYNC fica dentro da faixa do VRR.

**Limita o FPS por dentro do jogo.** Cobre DirectDraw, Direct3D 8 a 12, OpenGL e Vulkan (sem layer e sem reiniciar o jogo), em 64 e 32 bits, com precisão de 0,01 FPS.
- **Descarga limpa:** sai da memória do jogo ao parar.
- **Uma cópia só:** nunca empilha duas cópias de si mesmo no mesmo jogo.
- **Anti-cheat:** recusa jogos protegidos, sem exceção.
- **Padrão:** taxa fixa, e só entra quando você pede.
- **Com FG:** os modos ligados ao monitor (taxa, VRR −3%, metade, casar com a geração) dão ao jogo a taxa da tela dividida pelo multiplicador. Limitador e FG nunca se atropelam.

**Faz upscaling dentro do jogo (FlowScale SR).** Assume o lugar do FSR 3.1 ou do XeSS em jogos DirectX 12 e reconstrói a imagem com a profundidade, os vetores de movimento e o jitter do próprio jogo. Inclui o Relight, com oclusão ambiente e um rebote de luz. Também instala como provedor do OptiScaler.
- **Achar os jogos:** Steam, Epic, GOG, Xbox, Heroic e pastas suas.
- **Instalar e restaurar:** sempre com backup; restaurar deixa a pasta idêntica byte a byte.
- **Ajustes ao vivo:** o que você muda no app vale na hora, com o jogo aberto.

**Mostra o que está acontecendo.** O overlay no jogo traz FPS, quadros gerados, tempo de quadro, 1% mínima e gráfico, também com só o limitador ligado, e não aparece em gravações. A tela inicial mostra o gráfico ao vivo e o estado do detector de jogos.

**Do seu jeito.** Perfis por jogo, por caminho ou por título de janela, com curingas; um perfil de launcher vale para o jogo que ele abre. Tem atalhos, tema claro, escuro ou do Windows, e interface em português, inglês e espanhol.

## Medido

GTX 1650, monitor de 180 Hz, Super Mario Odyssey (port para PC, Vulkan):

| Cenário | Jogo | Na tela | p99 |
|---|---|---|---|
| Limitador a 60 | 60,00 FPS, σ 0,000 ms | | |
| Escala sem geração | 60,3 | 60,3 | 16,9 ms |
| Geração 2× | 58,5 | 116,6 | 14,4 ms |
| Geração 3×, qualidade automática | 59,0 | 176,2 | 13,8 ms |

| Teste | Resultado |
|---|---|
| Limitador em D3D9, D3D11, OpenGL e DirectDraw, 64 e 32 bits | alvo exato, σ ≤ 0,011 ms |
| Godot 4 em Vulkan, D3D12 e OpenGL | 72,00 FPS nos três |
| Geração de quadros (`-fgbench`, média de 5 cenas) | 34,9 dB, contra 20,0 de repetir quadros |
| Escala 2× (`-scalebench`) | Anime4K UL 38,4 dB, Lanczos 37,1, NIS 35,1 |
| FlowScale SR em 1080p | cerca de 1,5 ms de GPU |

## Compilar

```powershell
powershell -ExecutionPolicy Bypass -File build.ps1
```

Precisa de Rust (MSVC), Visual Studio Build Tools, Windows SDK e Node. O resultado fica em `dist\`, com o zip de release ao lado. Cada push no `main` é compilado e testado pelo GitHub Actions, sem usar o storage do Actions. Quando a versão do `Cargo.toml` sobe e ganha a sua seção no [CHANGELOG](CHANGELOG.md), a release sai sozinha, com essas notas.

## Linha de comando

```text
flowscale.exe -minimized | -page limiter | -devtools
flowscale.exe -scale "título" [seg]               escala uma janela
flowscale.exe -limit "título" <fps> <seg> [detach] limita, mede e grava o resultado no log
flowscale.exe -game <pasta> install|restore        instala ou restaura o FlowScale SR num jogo
flowscale.exe -games                               lista os jogos encontrados
flowscale.exe -selftest                            shaders, geração, rotação e overlay
flowscale.exe -fgbench | -fgcost | -scalebench     qualidade e custo, medidos
flowscale.exe -osdshot <arquivo.bmp>               renderiza o overlay em todos os estilos
flowscale.exe -gsync status|on|off                 perfil de G-SYNC no driver NVIDIA
flowscale.exe --uninstall-cleanup                  desfaz tudo que o app configurou no sistema
```

## Onde está cada coisa

`src/engine.rs` captura, escala, gera e apresenta. `src/scale.rs` tem os escaladores e `src/shaders.hlsl` os shaders. `src/hook.rs` é a DLL que roda no jogo, `src/limiter.rs` injeta e detecta, `src/ipc.rs` liga os dois. `src/main.rs` é o app e `ui/src/` a interface. `src/ingame.rs` cuida da instalação do SR, cujo código vive em `sr/`. `src/config.rs` define todos os ajustes num lugar só.

## Limites

- A geração é fluxo óptico, sem rede neural: movimentos muito rápidos e transparências podem deixar artefatos.
- Numa GPU de entrada, a geração disputa a GPU com o jogo. A qualidade automática protege o FPS do jogo à custa de alguma nitidez em movimento.
- O FlowScale SR entra só por DirectX 12. Vulkan, DLSS e FSR 2 ficam de fora.
- Jogos com anti-cheat não são tocados, de propósito.
- O Direct3D 8 e os monitores girados foram validados em teste, não em hardware real.

## Créditos

O FlowScale SR parte do núcleo do DittoSR (Vancete), derivado do Snapdragon GSR 2, e o Relight usa ideias do CORTAO. Os escaladores incluem NVIDIA Image Scaling, Anime4K (bloc97), xBR (Hyllian) e FSR 1, CAS e RCAS da AMD. Todos são MIT ou BSD, com as licenças em `licenses/`.

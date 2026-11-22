# Novidades

Cada versão nova entra aqui, no topo. A release só é publicada quando a versão do `Cargo.toml` muda e tem a sua seção neste arquivo: o texto dela vira as notas da release.

## 2.1.0

**Limitador e geração de quadros, juntos**
- O limite padrão agora é uma taxa fixa, e o limitador só entra quando você pede.
- Os modos ligados ao monitor (taxa, VRR −3%, metade, casar com a geração) dão ao jogo a taxa da tela dividida pelo multiplicador do FG.
- A saída do FG nunca passa do monitor e, com G-SYNC, fica dentro da faixa do VRR.
- Mudar um ajuste com a escala ativa reinicia a sessão no mesmo jogo, então motor e limitador nunca divergem.

**Corrigido em jogo real** (Super Mario Odyssey, Vulkan, GTX 1650, 180 Hz)
- Duas cópias do limitador no mesmo jogo derrubavam o FPS pela metade e podiam fechar o jogo. Agora só uma cópia age por jogo: limite a 60 segura 60,00 FPS, σ 0,000 ms.
- Quadros Vulkan e OpenGL que o driver reapresenta pelo DXGI contam uma vez só.
- Geração 3× com VSync em janela: de 7,7 para 59 FPS reais, 176 na tela.

**Geração de quadros**
- Qualidade automática: mede o custo na GPU e baixa a resolução do fluxo até 40% antes de tirar FPS do jogo.
- Cortes de cena não são mais misturados.

**FlowScale SR**
- Relight novo, com passes em meia resolução, filtro CACAO, curva de tons e fade por distância.
- Instala como provedor do OptiScaler, e os ajustes valem na hora com o jogo aberto.

**App**
- Estado do detector de jogos na tela inicial, e perfis de launcher valendo para o jogo que ele abre.
- Rotação de monitor corrigida, com teste pixel a pixel.
- Log com histórico compactado, estatísticas de ritmo por sessão e registro automático de travamentos da interface.

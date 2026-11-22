# Contribuindo

Correções, escaladores novos, traduções e relatos de jogos são bem-vindos. Forks também.

## Rodando localmente

```powershell
powershell -ExecutionPolicy Bypass -File build.ps1   # tudo em dist\
cargo test --release                                 # testes
dist\flowscale.exe -selftest                         # shaders, geração, rotação e overlay
dist\flowscale.exe -fgbench                          # qualidade da geração de quadros
dist\flowscale.exe -scalebench                       # qualidade e custo dos escaladores
```

## Combinados

- **Números antes de opinião.** Mudança de desempenho ou qualidade vem com medida: `-fgbench`, `-scalebench`, a linha `session:` que o log grava a cada sessão de escala, ou a linha `limit:` de `flowscale.exe -limit "<título>" <fps> <seg> detach`.
- **O jogo nunca pode cair.** A DLL do jogo (`src/hook.rs`) passa direto quando em dúvida, sai da memória limpa e é a única cópia instalada em cada processo.
- **Nada de anti-cheat.** O detector recusa esses jogos de propósito.
- **Nada copiado de ferramentas fechadas.** Estudar comportamento pode, copiar não.
- **Interface em três idiomas.** Todo texto novo vai em português, inglês e espanhol, em `ui/src/index.html`.
- **Ajustes num lugar só.** Todo ajuste novo entra no esquema de `src/config.rs`.

## Versões

Para publicar, suba a versão no `Cargo.toml` e escreva a seção dela no topo do `CHANGELOG.md`. O push no `main` faz o resto.

## Pull requests

Um assunto por PR, com o que mudou e como foi medido. Os testes e o `-selftest` precisam passar. Para relatar um jogo, mande o nome dele, a GPU, o fim do `flowscale.log` e o que você viu.

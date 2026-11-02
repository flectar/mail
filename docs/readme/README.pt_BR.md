<div align="center">
  <img src="../../resources/app-icon/flectar-mail-masked.png" width="88" alt="Logotipo do Flectar Mail">
  <h1 align="center">
    Flectar Mail
  </h1>
  <p>
    <a href="../../README.md">English</a> · <a href="README.es.md">Español</a> · <a href="README.de.md">Deutsch</a> ·
    <a href="README.fr.md">Français</a> · <strong>Português (Brasil)</strong> · <a href="README.ru.md">Русский</a> ·
    <a href="README.hi.md">हिन्दी</a> · <a href="README.ar.md">العربية</a> · <a href="README.zh_Hans.md">简体中文</a>
  </p>
  <div align="center">
    <h3>E-mail rápido de novo</h3>
    <p>Criado do zero para ser rápido. O Flectar Mail oferece desempenho nativo, início instantâneo e consumo de RAM a partir de apenas 80 MB.</p>
  </div>
  <p>
    <a href="https://flectar.com">Site</a> ·
    <a href="https://github.com/flectar/mail/issues">Relatar um problema</a> ·
    <a href="../../CONTRIBUTING.md">Contribuir</a>
  </p>
  <p>
    <a href="../../LICENSE"><img src="https://img.shields.io/github/license/flectar/mail?style=flat&label=license&color=2563eb" alt="Licença: AGPL-3.0"></a>
    <a href="https://github.com/flectar/mail/releases"><img src="https://img.shields.io/github/downloads/flectar/mail/total?style=flat&label=downloads&color=16a34a" alt="Downloads de todas as versões"></a>
    <a href="https://github.com/flectar/mail/releases"><img src="https://img.shields.io/github/v/release/flectar/mail?display_name=tag&include_prereleases&sort=semver&style=flat&label=release&color=0ea5e9" alt="Última versão"></a>
    <a href="https://translate.flectar.com/engage/flectar-mail/"><img src="https://translate.flectar.com/widget/flectar-mail/svg-badge.svg" alt="Status da tradução"></a>
  </p>
  <p>
    <a href="https://www.rust-lang.org/"><img src="https://img.shields.io/badge/language-Rust%201.92%2B-dea584?style=flat" alt="Linguagem: Rust 1.92+"></a>
    <img src="https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux%20%7C%20Android%20%7C%20iOS-475569?style=flat" alt="Plataformas: Windows, macOS, Linux, Android, iOS">
  </p>
</div>

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-light.png">
  <img src="../../resources/screenshots/desktop-light.png" alt="Flectar Mail no macOS com a caixa de entrada unificada e a visualização da mensagem">
</picture>

O Flectar Mail é um aplicativo nativo e leve para seus e-mails, calendários e contatos. Foi projetado para abrir instantaneamente, responder com agilidade e usar uma fração da memória de um cliente de e-mail típico baseado em tecnologias web.

## ❓ Por que usar o Flectar Mail?

- **Rápido desde o primeiro clique.** A interface nativa e o acesso a dados que prioriza o armazenamento local levam você à caixa de entrada sem esperar por um ambiente de navegador.
- **A partir de apenas 80 MB de RAM.** O Flectar Mail foi projetado para consumir pouca memória, mesmo com uma caixa de entrada cheia de recursos.
- **Tudo em um só lugar.** Alterne entre e-mails, calendários e contatos sem precisar combinar aplicativos separados.
- **Feito para funcionar offline.** A caixa de e-mail e o calendário ficam armazenados localmente, então os dados sincronizados continuam úteis sem conexão.
- **Compatível com suas contas.** Conecte Gmail, Outlook e Microsoft 365 ou serviços baseados nos padrões IMAP, POP3, SMTP, JMAP, CalDAV e CardDAV.
- **Privacidade por padrão.** Imagens remotas ficam bloqueadas até sua autorização, ajudando a impedir que pixels de rastreamento revelem quando você lê uma mensagem.
- **Segurança na arquitetura.** O conteúdo dos e-mails nunca é aberto em uma WebView. O Flectar Mail renderiza HTML e CSS com seu próprio sistema nativo em Rust, não executa scripts dos e-mails e bloqueia imagens remotas por padrão. Isso evita, desde o projeto, a superfície de ataque de um navegador incorporado.
- **Um renderizador experimental em Rust.** Criar um renderizador de e-mails sem um motor de navegador é um novo território. Problemas de renderização são esperados, especialmente em mensagens complexas, enquanto a compatibilidade melhora.
- **Para todas as telas.** Os layouts de desktop, completo e minimalista, compartilham a mesma experiência da interface compacta adaptada ao toque.
- **Nativo e de código aberto.** Criado do zero em Rust. Não é um navegador dentro de uma janela e é distribuído sob a AGPLv3.

## 📁 Arquivos e anexos

Explore o armazenamento JMAP/WebDAV, pesquise anexos de e-mail, mantenha arquivos offline e visualize PDFs, imagens e texto.

## ✍🏻 Assinaturas de conta e OpenPGP

Configurações → Contas → Assinaturas e OpenPGP oferece assinaturas com nome, padrões separados para novas mensagens e respostas e um seletor de assinatura ao escrever. A assinatura e a criptografia OpenPGP/MIME no desktop usam o GnuPG 2.x instalado, com pinentry para as frases secretas das chaves privadas. A criptografia obrigatória bloqueia o envio quando as chaves dos destinatários estão ausentes ou são inválidas; rascunhos protegidos permanecem locais até o envio.

S/MIME e OpenPGP em dispositivos móveis ainda não são suportados.

## 🧪 Renderização HTML experimental

> [!WARNING]
> A renderização de e-mails HTML é atualmente a parte mais experimental do Flectar Mail. Algumas mensagens, especialmente aquelas com marcação e CSS complexos ou incomuns, podem não ser exibidas corretamente ainda.

Para manter o cliente totalmente nativo e o consumo de memória em torno de 80 MB, o Flectar Mail renderiza o HTML dos e-mails com o [Blitz](https://github.com/DioxusLabs/blitz), um renderizador HTML/CSS em Rust da equipe Dioxus, em vez de incorporar um navegador ou WebView.

As versões de desktop incluem **CPU — Low Memory** e **GPU — WGPU** em Configurações → Geral → Renderizador. A CPU é selecionada inicialmente e não inicializa o WGPU. A opção GPU usa Slint e Vello em um dispositivo WGPU 30 compartilhado; a alteração entra em vigor após reiniciar o aplicativo. Se a inicialização com GPU falhar, o aplicativo volta automaticamente à CPU.

Até onde sabemos, o Flectar Mail é um dos primeiros projetos a usar o Blitz para HTML arbitrário de e-mails reais. As mensagens contêm muito HTML e CSS incomuns, impondo desafios ao renderizador. Atualmente mantemos várias correções sobre o Blitz e esperamos incorporar o máximo possível desse trabalho ao projeto original ao longo do tempo.

Se encontrar um e-mail exibido incorretamente, [relate o problema](https://github.com/flectar/mail/issues). Essa abordagem ainda é experimental, mas também é uma das principais razões pelas quais o Flectar Mail pode ser tão leve em comparação com clientes baseados em WebView criados com ferramentas como Tauri ou Wails.

## 🎨 Do seu jeito

Escolha o espaço de trabalho que combina com sua maneira de lidar com e-mails. Mantenha o layout detalhado de três painéis, mude para uma visão minimalista, escolha um tema claro ou escuro, selecione uma paleta incluída ou crie a sua e mostre ou oculte os avatares dos remetentes. Você também pode dar outra cor ao ícone do aplicativo: na barra de tarefas ou no Dock, na bandeja do sistema, no inicializador de aplicativos do Linux e na tela inicial do Android e do iOS.

A aparência das mensagens pode seguir o tema do aplicativo ou permanecer clara ou escura de forma independente. A paleta selecionada se aplica ao HTML, ao texto simples e ao modo de leitura. Para mostrar temporariamente as cores originais do remetente, use o ícone de contraste entre a busca e o modo de leitura na barra de ferramentas da mensagem no desktop, ou escolha **Mostrar cores originais** no menu de três pontos da mensagem em qualquer tamanho de tela.

O espaço de trabalho completo mantém suas pastas, a lista de mensagens e o e-mail selecionado visíveis ao mesmo tempo. O layout minimalista reduz a poluição visual e dá mais espaço a cada parte da caixa de entrada quando necessário.

### 👥 Perfis e cores das contas

Agrupe contas relacionadas em perfis com nomes como Trabalho ou Pessoal e dê uma cor a cada perfil. Crie um perfil pelo menu no início da barra de abas ou em Configurações → Contas → Perfis e marque as contas que pertencem a ele. Escolha um perfil nesse menu para concentrar a visualização nas suas contas: a caixa de entrada unificada, a busca, os contadores e a barra lateral mostram apenas os e-mails dessas contas, enquanto as abas permanecem iguais. **Todas as contas** volta a mostrar tudo.

Você também pode substituir a cor de uma conta individual e mostrar a cor resultante na borda esquerda de cada mensagem. Os marcadores facilitam distinguir contas na caixa de entrada unificada; o exemplo abaixo usa roxo para Trabalho e laranja para Suporte.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-profiles-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-profiles-light.png">
  <img src="../../resources/screenshots/desktop-profiles-light.png" alt="Caixa de entrada unificada do Flectar Mail com marcadores de perfil roxos e laranjas">
</picture>

### 🧵 Conversas

As respostas ficam agrupadas em ordem cronológica, com a mensagem ativa expandida no painel de leitura.

Use **Cima/Baixo** para navegar pelas mensagens, **Home/End** para ir à primeira ou à última mensagem carregada e **Page Up/Page Down** para avançar uma página por vez. **Enter** abre a mensagem atual e leva o foco ao conteúdo; **Espaço** marca ou desmarca a mensagem para ações em lote. **Tab/Shift+Tab** alternam entre controles. No espaço completo, **F6** move o foco entre a lista e o leitor. No layout minimalista, as setas destacam mensagens e **Enter** as abre; **Escape** ou **F6** retorna à lista.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-thread-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-thread-light.png">
  <img src="../../resources/screenshots/desktop-thread-light.png" alt="Conversa do Flectar Mail com mensagens recebidas e respostas">
</picture>

### 🗂️ Abas

Mantenha vários espaços abertos ao mesmo tempo com abas como as de um navegador. Cada aba lembra sua localização: uma caixa de e-mail, busca ou conversa aberta, o calendário, os contatos, os arquivos ou as configurações, além da posição da rolagem e da mensagem que você estava lendo. Clique com o botão do meio ou com Ctrl/⌘ em uma mensagem para abri-la em uma aba em segundo plano (adicione Shift para mudar para ela), ou escolha **Abrir em nova aba** no menu. Arraste abas para reordená-las, feche com o botão do meio e use o botão direito para fechar as demais ou as que estão à direita. Continue de onde parou na próxima vez que abrir o Flectar Mail.

**Ctrl/⌘+T** abre uma nova aba, **Ctrl/⌘+W** fecha a atual e **Ctrl/⌘+Shift+T** reabre a última aba fechada. **Ctrl+Tab** e **Ctrl+Shift+Tab** alternam entre abas, **Ctrl/⌘+1–8** vão para uma aba específica e **Ctrl/⌘+9** vai para a última.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-tabs-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-tabs-light.png">
  <img src="../../resources/screenshots/desktop-tabs-light.png" alt="Flectar Mail com abas de várias conversas, calendário, contatos e arquivos">
</picture>

### Espaços de trabalho completo e minimalista

<table>
  <thead>
    <tr>
      <th width="50%">Espaço completo</th>
      <th width="50%">Espaço minimalista</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-light.png">
          <img src="../../resources/screenshots/desktop-light.png" alt="Espaço de trabalho completo do Flectar Mail no desktop" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-minimal-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-minimal-light.png">
          <img src="../../resources/screenshots/desktop-minimal-light.png" alt="Espaço de trabalho minimalista do Flectar Mail no desktop" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### Paletas de cores

<table>
  <thead>
    <tr>
      <th width="25%">Verde-azulado</th>
      <th width="25%">Verde</th>
      <th width="25%">Roxo</th>
      <th width="25%">Personalizada</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-teal-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-teal-light.png">
          <img src="../../resources/screenshots/desktop-teal-light.png" alt="Paleta verde-azulada do Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-green-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-green-light.png">
          <img src="../../resources/screenshots/desktop-green-light.png" alt="Paleta verde do Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-purple-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-purple-light.png">
          <img src="../../resources/screenshots/desktop-purple-light.png" alt="Paleta roxa do Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="25%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-light.png">
          <img src="../../resources/screenshots/desktop-light.png" alt="Paleta personalizada do Flectar Mail" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### 🪟 Janela sem moldura

Ative **Janela sem moldura** em Configurações → Geral → Interface para substituir a barra de título do sistema por controles na barra de ferramentas. Cada plataforma mantém seu comportamento: macOS preserva os botões coloridos nativos, Windows mantém Snap Layouts e o menu do sistema, e Linux segue a ordem dos botões e as ações da barra de título configuradas no GNOME ou KDE.

<table>
  <tbody>
    <tr>
      <th width="50%">macOS</th>
      <th width="50%">Windows</th>
    </tr>
    <tr>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/frameless/macos-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/frameless/macos-light.png">
          <img src="../../resources/screenshots/frameless/macos-light.png" alt="Janela sem moldura do Flectar Mail no macOS" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/frameless/windows-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/frameless/windows-light.png">
          <img src="../../resources/screenshots/frameless/windows-light.png" alt="Janela sem moldura do Flectar Mail no Windows" width="100%">
        </picture>
      </td>
    </tr>
    <tr>
      <th width="50%">Linux (GNOME)</th>
      <th width="50%">Linux (KDE)</th>
    </tr>
    <tr>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/frameless/linux-gnome-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/frameless/linux-gnome-light.png">
          <img src="../../resources/screenshots/frameless/linux-gnome-light.png" alt="Janela sem moldura do Flectar Mail no GNOME" width="100%">
        </picture>
      </td>
      <td width="50%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/frameless/linux-kde-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/frameless/linux-kde-light.png">
          <img src="../../resources/screenshots/frameless/linux-kde-light.png" alt="Janela sem moldura do Flectar Mail no KDE Plasma" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### 📅 Calendário, contatos e arquivos

<table>
  <thead>
    <tr>
      <th width="33.33%">Calendário</th>
      <th width="33.33%">Contatos</th>
      <th width="33.33%">Arquivos (WebDAV/JMAP)</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-calendar-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-calendar-light.png">
          <img src="../../resources/screenshots/desktop-calendar-light.png" alt="Calendário do Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-contacts-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-contacts-light.png">
          <img src="../../resources/screenshots/desktop-contacts-light.png" alt="Contatos do Flectar Mail" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-files-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-files-light.png">
          <img src="../../resources/screenshots/desktop-files-light.png" alt="Arquivos do Flectar Mail" width="100%">
        </picture>
      </td>
    </tr>
  </tbody>
</table>

### 💾 Armazenamento e backups

Configurações → Armazenamento mostra quanto espaço os e-mails, anexos, arquivos offline e bancos de dados usam no dispositivo. Nessa mesma página, você pode exportar instantâneos verificados dos bancos de dados ou transferir as configurações de contas conectadas e as preferências por meio de um backup. Senhas e tokens OAuth ficam no chaveiro do sistema, portanto um dispositivo restaurado solicita um novo login.

Contas POP3 baixam e-mails para este dispositivo e os deixam no servidor, a menos que você escolha outra opção: cada conta pode remover e-mails do servidor após 7, 14 ou 30 dias, quando você os exclui aqui ou assim que são baixados, mas nunca antes de serem armazenados no dispositivo. Suas pastas só existem neste dispositivo, por isso os instantâneos incluem esses e-mails. Ao remover uma conta POP3 ou excluir todos os dados, o aplicativo oferece primeiro exportá-los como arquivos mbox, um por pasta, que outros aplicativos de e-mail podem importar.

A mesma importação também traz contas do Thunderbird (uma pasta de perfil, seu arquivo de Ferramentas › Exportar ou `prefs.js`), do Thunderbird para Android ou do K-9 Mail (exportações de configurações `.k9s`). Você revisa cada conta antes de qualquer adição: configurações que o Flectar Mail não pode reproduzir, como um servidor Exchange, são explicadas em vez de alteradas silenciosamente, e as contas POP3 mantêm sua regra de remoção de e-mails do servidor. Contas existentes nunca são modificadas e senhas nunca são importadas.

E-mails salvos também podem ser importados: as Pastas locais do Thunderbird e os e-mails que suas contas POP3 baixaram, arquivos mbox como uma exportação do Google Takeout ou os do próprio Flectar Mail, mensagens `.eml`, pastas maildir, caixas de correio do Apple Mail e arquivos ZIP que os contenham. Você escolhe os arquivos, vários de uma vez se quiser, e depois as pastas (Lixeira e Spam ficam de fora, a menos que você as marque), e a estrutura de pastas e os estados de lido e com estrela são mantidos. E-mails que já estão lá são ignorados, então uma importação interrompida pode simplesmente ser executada de novo, e uma importação longa pode continuar em segundo plano. E-mails que não pertencem a nenhuma conta vão para **Neste dispositivo**: pastas que existem só aqui e que Configurações → Armazenamento pode exportar ou excluir. Eles também podem ir para as pastas de uma conta POP3 ou ser enviados ao servidor de uma conta IMAP, JMAP ou Gmail, em uma pasta própria.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-storage-dark.png">
  <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-storage-light.png">
  <img src="../../resources/screenshots/desktop-storage-light.png" alt="Configurações de armazenamento do Flectar Mail com uso local, e-mails neste dispositivo, instantâneos dos bancos de dados e backups das contas">
</picture>

### 📱 Feito para telas menores

A interface compacta mantém as ações importantes ao alcance e dá a tela inteira às mensagens, aos eventos e aos contatos quando necessário.

| Claro | Escuro |
| --- | --- |
| ![Caixa de entrada móvel do Flectar Mail no modo claro](../../resources/screenshots/mobile-light.png) | ![Caixa de entrada móvel do Flectar Mail no modo escuro](../../resources/screenshots/mobile-dark.png) |

## 📥 Baixe o Flectar Mail

O Flectar Mail está em desenvolvimento e ainda não é estável.

> [!WARNING]
> **A verificação OAuth do Google exige uma avaliação anual de segurança CASA.** O Flectar é um projeto comunitário de código aberto, então essa avaliação depende do apoio de patrocinadores. Até que a verificação necessária seja financiada e concluída, não será possível oferecer uma versão estável com OAuth do Gmail configurado por padrão. Para ajudar a disponibilizar o login integrado do Gmail, [patrocine o Flectar](https://github.com/sponsors/flectar).
>
> A verificação OAuth do Google e da Microsoft ainda está em andamento. As versões preliminares anteriores do GitHub são destinadas a testes com registros OAuth próprios ou contas IMAP, POP3 ou JMAP.

Compilações sem chaves de aplicativo OAuth mantêm o login do Gmail e Outlook desativado. Use a engrenagem de **Configurações de login** na tela de boas-vindas para salvar seu próprio registro de aplicativo do Google ou da Microsoft, ou conecte uma conta IMAP, POP3 ou JMAP. As chaves personalizadas são compartilhadas com Configurações e têm prioridade sobre as incluídas; limpar um ID de cliente personalizado restaura os padrões quando disponíveis. Cada provedor é ativado separadamente após salvar sua configuração.

Quando versões de teste forem publicadas, baixe-as em [GitHub Releases](https://github.com/flectar/mail/releases) e procure a indicação **Pre-release**:

- **Linux x64:** AppImage, `.deb` para Debian/Ubuntu, `.rpm` para Fedora 43/44 ou pacote Flatpak de teste para instalação manual
- **Windows x64:** Instalador `.exe` ou ZIP portátil
- **macOS Apple silicon ou Intel (macOS 14+):** DMG ou ZIP do aplicativo
- **Android arm64 (Android 8.0+):** APK assinado; versões de teste e estáveis compartilham uma identidade de aplicativo

As versões de teste do Windows não são assinadas; as do macOS têm assinatura ad hoc e não são notarizadas, então avisos de segurança do sistema operacional são esperados. Instale as atualizações manualmente.

A versão de teste do Flatpak é fornecida como um pacote independente. Ainda não é um pacote do Flathub e, portanto, não recebe atualizações automáticas do Flathub. A opção de fechar para a bandeja está desativada até que o componente da bandeja possa usar um nome D-Bus compatível com o ambiente isolado. O início automático ao entrar na sessão usa o portal Background do desktop e abre a janela, pois não há bandeja para iniciar minimizado. Nele, o ícone do aplicativo continua laranja, porque o ambiente isolado não pode alterar o ícone do inicializador no sistema.

Instale o pacote Linux baixado com `sudo dnf install ./flectar-mail-<version>-linux-x64.rpm` ou `flatpak install --user ./flectar-mail-<version>-linux-x64.flatpak`. Inicie o Flatpak com `flatpak run com.flectar.mail`.

O RPM é destinado ao Fedora 43 e 44 em x86_64. As compilações de lançamento usam o Fedora 43 como base de compatibilidade; o mesmo RPM deve passar por verificações de instalação, renderização de PDF, inicialização em X11 e Wayland e remoção em contêineres limpos das duas versões antes da publicação. Outras distribuições baseadas em RPM ainda não foram testadas. Se o Discover informar um conflito genérico, execute o comando `dnf install` acima e inclua o erro de dependência no relato do problema.

O RPM anterior, `0.1.0-alpha.7`, foi compilado no Fedora 44 e não pode ser instalado no Fedora 43 porque exige glibc 2.43. Seu pacote Flatpak é uma alternativa enquanto se aguarda uma versão RPM criada com as verificações de compatibilidade acima.

Cada lançamento no GitHub inclui `SHA256SUMS` e atestados assinados de procedência da compilação. Com o GitHub CLI instalado, verifique um download com `gh attestation verify <download> --repo flectar/mail`.

A partir da alpha 8, as versões alpha, beta, candidatas a lançamento e estáveis do Android usam um único pacote (`com.flectar.mail`) e uma identidade de assinatura persistente. Versões posteriores podem atualizar o aplicativo e preservar os dados locais. Alpha 6 e alpha 7 usavam chaves diferentes e não podem atualizar para essa identidade de assinatura.

Os dois fluxos de lançamento do Android usam o ambiente GitHub `android-release`. Exigem os mesmos segredos de armazenamento de chaves e senhas, verificam o certificado público com `platform/android/app-signing-certificate.sha256` e derivam os códigos de versão do Android da versão do lançamento. Uma chave ausente ou diferente bloqueia a publicação. **Não gere uma chave substituta nem altere a impressão digital fixada em um lançamento de rotina.** Alterações de senha preservam a identidade quando a chave privada e o certificado originais são mantidos.

Consulte a [configuração e recuperação da assinatura Android](../android-signing.md) para o comando de migração executado uma única vez, os segredos necessários, os backups e o Play App Signing.

Compilações locais de desenvolvimento ainda podem gerar uma chave de teste local. Elas só atualizam instalações assinadas com essa mesma chave.

Você também pode compilar o Flectar Mail a partir do código-fonte com as [ferramentas do Rust](https://rustup.rs/):

```bash
cargo run --bin flectar-mail
```

O OAuth no desktop Linux usa o navegador do sistema por meio do portal do desktop e armazena as credenciais de renovação pelo freedesktop Secret Service. Uma sessão normal precisa, portanto, de um componente `xdg-desktop-portal` e de um provedor de Secret Service como o GNOME Keyring. Compilações de lançamento interrompem o processo antes de abrir a página OAuth quando o armazenamento seguro de credenciais não está disponível, para que a autorização nunca seja concluída sem um local seguro para persistir os dados.

Ambientes de desenvolvimento Podman e Docker geralmente não têm acesso ao D-Bus da sessão do host nem à rede de loopback do navegador do host. Compilações de depuração suportam esse cenário com um arquivo de credenciais de desenvolvimento claramente identificado, acessível somente pelo proprietário, e um painel expansível para colar a URL final de retorno de loopback. O armazenamento em arquivo é excluído das compilações de lançamento.

## 🌍 Ajude a traduzir o Flectar Mail

O Flectar Mail é feito para todos, e adoraríamos sua ajuda para disponibilizá-lo em mais idiomas!

Usamos o [Weblate](https://translate.flectar.com/) para gerenciar as traduções da comunidade. Seja para traduzir o Flectar Mail para seu idioma nativo, melhorar uma tradução existente ou revisar textos traduzidos, toda contribuição é bem-vinda.

**[Comece a traduzir o Flectar Mail →](https://translate.flectar.com/)**

É fácil começar:

1. Crie uma conta na nossa plataforma de tradução.
2. Selecione seu idioma ou inicie uma nova tradução se ele ainda não estiver disponível.
3. Traduza os textos diretamente no Weblate, sem precisar saber programar ou ter experiência com GitHub.

As traduções são sincronizadas com nosso repositório GitHub e enviadas como pull requests para revisão. As pessoas que contribuem também podem receber crédito pelo trabalho no GitHub.

As traduções do README são mantidas neste repositório. Consulte [Como contribuir com traduções](../../CONTRIBUTING.md#translations) para adicionar ou atualizar uma.

## ♿ Acessibilidade

O Flectar Mail inclui navegação por teclado, temas claros e escuros e layouts flexíveis. O trabalho de acessibilidade está em andamento. Consulte [ACCESSIBILITY.md](../../ACCESSIBILITY.md) para conhecer os recursos atuais, as limitações dos testes e como relatar uma barreira.

## 🔒 Segurança

Para relatar uma vulnerabilidade de forma privada, siga [SECURITY.md](../../SECURITY.md). Não publique detalhes de vulnerabilidades em issues públicas.

## 🌐 Código aberto para todos

O Flectar Mail é um único aplicativo de código aberto. Não existe uma edição comunitária separada. O cliente é licenciado sob a [GNU Affero General Public License v3](../../LICENSE). Leia a [visão geral do licenciamento](../../LICENSING.md) para os detalhes práticos ou consulte [CONTRIBUTING.md](../../CONTRIBUTING.md) para ajudar a construir o projeto.

## 🫡 Agradecimentos

O Flectar Mail é possível graças ao trabalho destes projetos e de seus colaboradores:

- [Slint](https://slint.dev/), o conjunto de ferramentas de interface nativa que dá vida à interface do Flectar Mail.
- [Blitz](https://github.com/DioxusLabs/blitz), o renderizador HTML/CSS em Rust da equipe Dioxus que permite a leitura dos e-mails.

---

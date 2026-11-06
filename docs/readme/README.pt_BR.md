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
    <p>Criado do zero para ser rápido. O Flectar Mail oferece desempenho nativo, início instantâneo e cerca de 20 MB de memória privada em repouso.</p>
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

| Benefício | Detalhes |
| --- | --- |
| **Rápido desde o primeiro clique** | A interface nativa e o acesso a dados que prioriza o armazenamento local levam você à caixa de entrada sem esperar por um ambiente de navegador. |
| **Cerca de 20 MB de memória privada em repouso** | Cerca de 85 MB contando o código e as bibliotecas, e doze vezes menos memória privada que o Thunderbird com a mesma caixa de e-mail. Consulte [Desempenho](#-desempenho). |
| **Tudo em um só lugar** | Alterne entre e-mails, calendários e contatos sem precisar combinar aplicativos separados. |
| **Feito para funcionar offline** | A caixa de e-mail e o calendário ficam armazenados localmente, então os dados sincronizados continuam úteis sem conexão. |
| **Compatível com suas contas** | Conecte Gmail, Outlook e Microsoft 365 ou serviços baseados nos padrões IMAP, POP3, SMTP, JMAP, CalDAV e CardDAV. |
| **Privacidade por padrão** | Imagens remotas ficam bloqueadas até sua autorização, então pixels de rastreamento não conseguem revelar quando você lê uma mensagem. |
| **Segurança na arquitetura** | O e-mail nunca é aberto em uma WebView. Um sistema nativo em Rust renderiza o HTML e o CSS sem executar os scripts dos e-mails, evitando a superfície de ataque de um navegador incorporado. |
| **Um renderizador experimental em Rust** | Renderizar e-mails sem um motor de navegador é um novo território. Mensagens complexas talvez ainda não sejam exibidas corretamente enquanto a compatibilidade melhora. |
| **Para todas as telas** | Os layouts de desktop, completo e minimalista, compartilham a mesma experiência da interface compacta adaptada ao toque. |
| **Nativo e de código aberto** | Criado do zero em Rust. Não é um navegador dentro de uma janela e é distribuído sob a AGPLv3. |

## ⚡ Desempenho

| Em repouso, mesma caixa de e-mail | Memória privada | Incluindo código e bibliotecas (PSS) | Renderiza e-mails com |
| --- | ---: | ---: | --- |
| **Flectar Mail** | **17,2–17,4 MiB** | **79,1–80,0 MiB** | Blitz |
| Geary 46 | 126,1–126,2 MiB | 309,6–311,4 MiB | WebKitGTK |
| Evolution 3.56 | 191,0–192,2 MiB | 395,0–395,9 MiB | WebKitGTK |
| Thunderbird 159 | 244,1–248,1 MiB | 424,4–427,1 MiB | Gecko |
| Betterbird 153 | 243,9–247,8 MiB | 411,9–419,7 MiB | Gecko |
| Mailspring 1.26 | 311,4–314,9 MiB | 551,1–551,7 MiB | Chromium |
| Aerion 0.3.6 | 421,9–437,3 MiB | 660,4–678,0 MiB | WebKitGTK |

Medições feitas no Linux com uma conta IMAP (860 mensagens), a caixa de entrada aberta e uma newsletter em HTML na tela, 60 segundos após abrir o aplicativo e com o renderizador por CPU, que é o padrão. Cada faixa abrange duas medições, e são contados todos os processos que cada aplicativo inicia. A memória privada é o que o Gerenciador de Tarefas do Windows mostra como **Memória**. No Windows, com 7 contas reais e cerca de 69.000 mensagens não lidas, o Gerenciador de Tarefas mostrou 37,8 MiB com a janela aberta e 4,2 MiB depois de fechá-la para a bandeja do sistema. Consulte [como medimos e como verificar por conta própria](../performance.md).

### Comparação com outros clientes

| Cliente de e-mail | Interface | Renderiza e-mails com | Plataformas |
| --- | --- | --- | --- |
| **Flectar Mail** | Aplicativo nativo em Rust | Blitz, um renderizador HTML/CSS em Rust; sem WebView | Windows, macOS, Linux, Android, iOS (via sideloading) |
| Thunderbird | Aplicativo de desktop baseado no Gecko | Gecko | Windows, macOS, Linux, Android |
| Outlook para Windows | Aplicativo do Windows com interface web | Microsoft Edge WebView2 | Windows |
| Spark | Aplicativo Electron | Chromium | macOS, Windows, iOS, Android |
| Aerion | Aplicativo em Go com interface web do Wails | WebView do sistema | Linux, macOS, Windows |
| Gmail em uma aba do navegador | Aplicativo web | Chrome ou outro navegador | Web |

## ✨ Recursos

| Recurso | O que faz |
| --- | --- |
| [Conversas](../features.md#conversation-threads) | Agrupa as respostas em ordem cronológica, com navegação completa por teclado. Você pode desativar esse recurso. |
| [Abas](../features.md#tabs) | Abas como as de um navegador que lembram onde você estava, com atalhos no desktop e um seletor de abas no celular. |
| [Perfis e cores das contas](../features.md#profiles-and-account-colors) | Agrupa contas em perfis como Trabalho ou Pessoal e identifica cada mensagem por cor. |
| [Aparência](../features.md#appearance) | Layout completo ou minimalista, tema claro ou escuro, paletas de cores, avatares dos remetentes e cores do ícone do aplicativo. |
| [Janela sem moldura](../features.md#frameless-window) | Controles de janela na barra de ferramentas que mantêm o comportamento de cada plataforma. |
| [Calendário, contatos e arquivos](../features.md#calendar-contacts-and-files) | Sincronização CalDAV e CardDAV, arquivos JMAP e WebDAV, pesquisa de anexos, arquivos offline e pré-visualizações. |
| [Assinaturas e OpenPGP](../features.md#signatures-and-openpgp) | Assinaturas com nome, além de assinatura e criptografia OpenPGP/MIME no desktop por meio do GnuPG. |
| [Armazenamento e backups](../features.md#storage-and-backups) | Espaço usado por tipo, instantâneos dos bancos de dados, backups das contas e regras de remoção de e-mails POP3 do servidor. |
| [Importação de contas e e-mails](../features.md#importing-accounts-and-mail) | Thunderbird, K-9 Mail, mbox, `.eml`, maildir, Apple Mail e arquivos ZIP. |
| [Renderização HTML](../features.md#html-rendering) | O HTML dos e-mails é renderizado em Rust com o Blitz, na CPU ou na GPU. |

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

### Abas, conversas e perfis

<table>
  <thead>
    <tr>
      <th width="33.33%">Abas</th>
      <th width="33.33%">Conversa</th>
      <th width="33.33%">Cores de perfil</th>
    </tr>
  </thead>
  <tbody>
    <tr>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-tabs-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-tabs-light.png">
          <img src="../../resources/screenshots/desktop-tabs-light.png" alt="Flectar Mail com abas de várias conversas, calendário, contatos e arquivos" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-thread-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-thread-light.png">
          <img src="../../resources/screenshots/desktop-thread-light.png" alt="Conversa do Flectar Mail com mensagens recebidas e respostas" width="100%">
        </picture>
      </td>
      <td width="33.33%">
        <picture>
          <source media="(prefers-color-scheme: dark)" srcset="../../resources/screenshots/desktop-profiles-dark.png">
          <source media="(prefers-color-scheme: light)" srcset="../../resources/screenshots/desktop-profiles-light.png">
          <img src="../../resources/screenshots/desktop-profiles-light.png" alt="Caixa de entrada unificada do Flectar Mail com marcadores de perfil roxos e laranjas" width="100%">
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

### Janela sem moldura

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

### Calendário, contatos e arquivos

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

### Feito para telas menores

| Claro | Escuro |
| --- | --- |
| ![Caixa de entrada móvel do Flectar Mail no modo claro](../../resources/screenshots/mobile-light.png) | ![Caixa de entrada móvel do Flectar Mail no modo escuro](../../resources/screenshots/mobile-dark.png) |

## 🧪 Renderização HTML experimental

> [!WARNING]
> A renderização de e-mails HTML é atualmente a parte mais experimental do Flectar Mail. Algumas mensagens, especialmente aquelas com marcação e CSS complexos ou incomuns, podem não ser exibidas corretamente ainda.

O Flectar Mail renderiza os e-mails com o [Blitz](https://github.com/DioxusLabs/blitz), um renderizador HTML/CSS em Rust da equipe Dioxus, em vez de incorporar um navegador ou WebView. Se uma mensagem for exibida incorretamente, [relate o problema](https://github.com/flectar/mail/issues). Saiba mais sobre [o renderizador e as opções de CPU e GPU](../features.md#html-rendering).

## 📥 Baixe o Flectar Mail

O Flectar Mail está em desenvolvimento e ainda não é estável. As versões de teste são publicadas em [GitHub Releases](https://github.com/flectar/mail/releases).

> [!WARNING]
> **A verificação OAuth do Google exige uma avaliação anual de segurança CASA.** O Flectar é um projeto comunitário de código aberto, então essa avaliação depende do apoio de patrocinadores. Até que a verificação necessária seja financiada e concluída, não será possível oferecer uma versão estável com OAuth do Gmail configurado por padrão. Para ajudar a disponibilizar o login integrado do Gmail, [patrocine o Flectar](https://github.com/sponsors/flectar).

| Plataforma | Download | Observações |
| --- | --- | --- |
| Linux x64 e arm64 | AppImage, `.deb` para Debian/Ubuntu, `.rpm` para Fedora 43/44 ou pacote Flatpak | Requer um chaveiro compatível com o Secret Service. O Flatpak é uma versão de teste para instalação manual e ainda não está no Flathub. |
| Windows x64 e arm64 | Instalador `.exe` ou ZIP portátil | Versões de teste não assinadas |
| macOS 14+ (Apple silicon ou Intel) | DMG ou ZIP do aplicativo | Assinatura ad hoc, sem notarização |
| Android 8.0+ (arm64) | APK assinado | Versões de teste e estáveis compartilham uma identidade de aplicativo. |
| iOS 17+ (iPhone e iPad) | `.ipa` sem assinatura | Instale com AltStore, SideStore ou Sideloadly usando seu ID Apple. |
| HarmonyOS e OpenHarmony | [Adaptação comunitária](../../platform/ohos/README.md) de [springmin](https://github.com/springmin) | Não faz parte dos lançamentos no GitHub; compile a partir do código-fonte. |

Compilações sem chaves de aplicativo OAuth mantêm o login do Gmail e Outlook desativado. Use a engrenagem de **Configurações de login** na tela de boas-vindas para adicionar seu próprio registro de aplicativo do Google ou da Microsoft, ou conecte uma conta IMAP, POP3 ou JMAP.

O [guia de instalação](../install.md) aborda os comandos de instalação, o chaveiro e o portal do desktop no Linux, observações sobre Flatpak e RPM, a verificação de downloads, a assinatura do Android, a instalação no iOS e a compilação a partir do código-fonte com `cargo run --bin flectar-mail`.

## 🤝 Participe

| Tópico | Detalhes |
| --- | --- |
| 🌍 **Traduzir** | Traduza o aplicativo, melhore uma tradução ou revise textos traduzidos no [Weblate](https://translate.flectar.com/), sem precisar saber programar ou ter experiência com GitHub. As traduções chegam a este repositório como pull requests, com crédito no GitHub para quem contribui. As traduções do README são mantidas aqui; consulte [Como contribuir com traduções](../../CONTRIBUTING.md#translations). |
| ♿ **Acessibilidade** | Navegação por teclado, temas claros e escuros e layouts flexíveis; o trabalho de acessibilidade está em andamento. O [ACCESSIBILITY.md](../../ACCESSIBILITY.md) descreve os recursos atuais, as limitações dos testes e como relatar uma barreira. |
| 🔒 **Segurança** | Relate vulnerabilidades de forma privada, conforme descrito em [SECURITY.md](../../SECURITY.md), e não publique os detalhes em issues públicas. |
| 🌐 **Código aberto** | Um único aplicativo de código aberto, sem uma edição comunitária separada, licenciado sob a [GNU Affero General Public License v3](../../LICENSE). A [visão geral do licenciamento](../../LICENSING.md) traz os detalhes práticos. |
| 🛠️ **Contribuir** | O [CONTRIBUTING.md](../../CONTRIBUTING.md) explica como ajudar a construir o projeto. |

## 🫡 Agradecimentos

O Flectar Mail é possível graças ao trabalho destes projetos e de seus colaboradores:

- [Slint](https://slint.dev/), o conjunto de ferramentas de interface nativa que dá vida à interface do Flectar Mail.
- [Blitz](https://github.com/DioxusLabs/blitz), o renderizador HTML/CSS em Rust da equipe Dioxus que permite a leitura dos e-mails.

---

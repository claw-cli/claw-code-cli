<div align="center">

# 🦀 Claude Code Rust

**Un runtime d'agent modulaire extrait de Claude Code, reconstruit en Rust.**

[![Statut](https://img.shields.io/badge/statut-conception-blue?style=flat-square)](https://github.com/)
[![Langage](https://img.shields.io/badge/langage-Rust-E57324?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Origine](https://img.shields.io/badge/origine-Claude_Code_TS-8A2BE2?style=flat-square)](https://docs.anthropic.com/en/docs/claude-code)
[![Licence](https://img.shields.io/badge/licence-MIT-green?style=flat-square)](../LICENSE)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen?style=flat-square)](https://github.com/)

[English](../README.md) | [简体中文](./README.zh-CN.md) | [日本語](./README.ja.md) | [한국어](./README.ko.md) | [Español](./README.es.md) | [Français](./README.fr.md)

<img src="./assets/overview.svg" alt="Vue d'ensemble du projet" width="100%" />

</div>

---

## 📖 Table des matières

- [Qu'est-ce que ce projet](#-quest-ce-que-ce-projet)
- [Pourquoi reconstruire en Rust](#-pourquoi-reconstruire-en-rust)
- [Objectifs de conception](#-objectifs-de-conception)
- [Architecture](#-architecture)
- [Détail des modules](#-détail-des-modules)
- [Rust vs TypeScript](#-rust-vs-typescript)
- [Feuille de route](#-feuille-de-route)
- [Structure du projet](#-structure-du-projet)
- [Contribuer](#-contribuer)
- [Références](#-références)
- [Licence](#-licence)

## 💡 Qu'est-ce que ce projet

Ce projet extrait les idées fondamentales du runtime agent de [Claude Code](https://docs.anthropic.com/en/docs/claude-code) et les réorganise en un ensemble de crates Rust réutilisables. Il ne s'agit pas d'une traduction ligne à ligne du TypeScript — c'est une refonte en salle blanche des capacités dont un agent dépend réellement :

- **Boucle de messages** — piloter des conversations multi-tours
- **Exécution d'outils** — orchestrer les appels d'outils avec validation de schéma
- **Contrôle des permissions** — autorisation avant accès fichiers / shell / réseau
- **Tâches longue durée** — exécution en arrière-plan avec gestion du cycle de vie
- **Compaction du contexte** — stabilité des longues sessions sous contraintes de tokens
- **Fournisseurs de modèles** — interface unifiée pour les backends LLM en streaming
- **Intégration MCP** — extension des capacités via le Model Context Protocol

Considérez-le comme un **squelette de runtime d'agent** :

| Couche | Rôle |
|--------|------|
| **Supérieure** | Un CLI léger qui assemble tous les crates |
| **Intermédiaire** | Runtime central : boucle de messages, orchestration des outils, permissions, tâches, abstraction du modèle |
| **Inférieure** | Implémentations concrètes : outils intégrés, client MCP, gestion du contexte |

> Si les frontières sont suffisamment nettes, cela peut servir non seulement aux agents de code façon Claude, mais à tout système d'agent qui a besoin d'une base runtime solide.

## 🤔 Pourquoi reconstruire en Rust

Claude Code est d'une excellente qualité d'ingénierie, mais c'est un **produit complet**, pas une bibliothèque de runtime réutilisable. Interface, runtime, systèmes d'outils et gestion d'état sont étroitement imbriqués. Lire le code source apprend beaucoup, mais en extraire des morceaux pour les réutiliser n'est pas trivial.

Ce projet vise à :

- **Décomposer** la logique fortement couplée en crates à responsabilité unique
- **Remplacer** les contraintes du runtime par des frontières `trait` et `enum`
- **Transformer** les implémentations du type « ne fonctionne qu'à l'intérieur de ce projet » en **composants d'agent réutilisables**

## 🎯 Objectifs de conception

1. **Runtime d'abord, produit ensuite.** Prioriser des fondations solides pour la boucle Agent, les outils, les tâches et les permissions.
2. **Chaque crate doit être auto-explicatif.** Les noms révèlent la responsabilité, les interfaces révèlent les frontières.
3. **Rendre le remplacement naturel.** Outils, fournisseurs de modèles, politiques de permission et stratégies de compaction doivent tous être interchangeables.
4. **Tirer parti de l'expérience de Claude Code** sans reproduire son interface ni ses fonctionnalités internes.

## 🏗 Architecture

<div align="center">
<img src="./assets/architecture.svg" alt="Vue d'ensemble de l'architecture" width="100%" />
</div>

### Carte des crates

| Crate | Objectif | Origine dans Claude Code |
|-------|----------|--------------------------|
| `agent-core` | Modèle de messages, conteneur d'état, boucle principale, session | `query.ts`, `QueryEngine.ts`, `state/store.ts` |
| `agent-tools` | Trait d'outil, registre, orchestration d'exécution | `Tool.ts`, `tools.ts`, couche service des outils |
| `agent-tasks` | Cycle de vie des tâches longues et mécanisme de notification | `Task.ts`, `tasks.ts` |
| `agent-permissions` | Autorisation des appels d'outils et correspondance des règles | `types/permissions.ts`, `utils/permissions/` |
| `agent-provider` | Interface de modèle unifiée, streaming, nouvelles tentatives | `services/api/` |
| `agent-compact` | Réduction du contexte et contrôle du budget de tokens | `services/compact/`, `query/tokenBudget.ts` |
| `agent-mcp` | Client MCP, connexion, découverte, reconnexion | `services/mcp/` |
| `tools-builtin` | Implémentations d'outils intégrés | `tools/` |
| `claude-cli` | Point d'entrée exécutable, assemble tous les crates | Couche CLI |

## 🔍 Détail des modules

<details>
<summary><b>agent-core</b> — Les fondations</summary>

Gère le démarrage, la poursuite et l'arrêt d'un tour de conversation. Définit le modèle de messages unifié, la boucle principale et l'état de session. C'est le socle de tout le système.
</details>

<details>
<summary><b>agent-tools</b> — Définition et dispatch des outils</summary>

Définit « à quoi ressemble un outil » et « comment les outils sont planifiés ». La version Rust évite de tout mettre dans un objet géant — les outils ne reçoivent que ce dont ils ont réellement besoin.
</details>

<details>
<summary><b>agent-tasks</b> — Runtime des tâches en arrière-plan</summary>

Séparer les appels d'outils des tâches du runtime est essentiel pour supporter les commandes longues, les agents en arrière-plan et les notifications de fin réinjectées dans la conversation.
</details>

<details>
<summary><b>agent-permissions</b> — Couche d'autorisation</summary>

Contrôle ce que l'agent peut faire, quand il doit demander à l'utilisateur et quand refuser net. Indispensable dès que l'agent lit, écrit ou exécute des commandes.
</details>

<details>
<summary><b>agent-provider</b> — Abstraction du modèle</summary>

Isole le système des différences entre backends de modèles. Unifie le streaming, la logique de nouvelles tentatives et la récupération d'erreurs.
</details>

<details>
<summary><b>agent-compact</b> — Gestion du contexte</summary>

Assure la stabilité des longues sessions. Pas seulement de la « synthèse » — applique différents niveaux de compression et contrôles de budget selon le contexte pour éviter une croissance sans limite.
</details>

<details>
<summary><b>agent-mcp</b> — Intégration MCP</summary>

Se connecte aux services MCP externes et intègre outils distants, ressources et prompts dans une surface de capacités unifiée.
</details>

<details>
<summary><b>tools-builtin</b> — Outils intégrés</summary>

Implémente les outils les plus courants, en priorisant fichiers, shell, recherche et édition — les opérations de base dont tout agent a besoin.
</details>

## ⚖️ Rust vs TypeScript

| TypeScript (Claude Code) | Approche Rust |
|--------------------------|---------------|
| Nombreuses vérifications à l'exécution | Reporter les vérifications dans le système de types |
| Les objets de contexte ont tendance à grossir sans limite | Contexte plus petit / frontières `trait` |
| Callbacks et événements éparpillés | Flux d'événements unifiés et continus |
| Feature flags à l'exécution | Gating par fonctionnalités à la compilation lorsque c'est possible |
| UI et runtime étroitement couplés | Runtime comme couche indépendante |

> Il ne s'agit pas de dire que Rust est « meilleur » — il est bien adapté pour **figer les frontières du runtime**. Pour un système d'agent qui évolue sur la durée, de telles contraintes ont en général de la valeur.

## 🗺 Feuille de route

<div align="center">
<img src="./assets/roadmap.svg" alt="Feuille de route" width="100%" />
</div>

### Phase 1 : Le faire fonctionner

- Mettre en place `agent-core`, `agent-tools`, `agent-provider`, `agent-permissions`
- Implémenter les outils de base `Bash`, `FileRead`, `FileWrite`
- Livrer un CLI minimal exécutable

> **Objectif :** une version de base capable de dialoguer, d'appeler des outils, d'exécuter des commandes et de lire/écrire des fichiers.

### Phase 2 : Stabiliser les sessions

- Ajouter `agent-tasks` pour les tâches en arrière-plan et les notifications
- Ajouter `agent-compact` pour les longues sessions et les gros résultats
- Étendre `tools-builtin` avec édition, recherche et sous-agents

> **Objectif :** des sessions qui tiennent plus longtemps sans devenir fragiles à cause de sorties surdimensionnées ou de tâches longues.

### Phase 3 : Ouvrir les frontières

- Intégrer `agent-mcp`
- Ajouter le chargement de plugins / skills
- Prendre en charge l'usage SDK / headless pour les scénarios embarqués

> **Objectif :** pas seulement un CLI, mais un runtime d'agent complet intégrable dans d'autres systèmes.

## 📁 Structure du projet

```text
rust-clw/
├── README.md                # Documentation en anglais
├── ARCHITECTURE.zh-CN.md    # Analyse d'architecture de Claude Code (TS)
└── docs/
    ├── README.zh-CN.md      # 简体中文文档
    ├── README.ja.md         # 日本語ドキュメント
    ├── README.ko.md         # 한국어 문서
    ├── README.es.md         # Documentación en español
    ├── README.fr.md         # Documentation en français
    └── assets/
        ├── overview.svg     # Schéma de vue d'ensemble du projet
        ├── architecture.svg # Schéma d'architecture
        └── roadmap.svg      # Schéma de feuille de route
```

> Une fois les crates en place, cela s'étendra en un workspace Rust complet.

## 🤝 Contribuer

Les contributions sont les bienvenues ! Ce projet en est à sa phase de conception initiale, et il existe de nombreuses façons d'aider :

- **Retours sur l'architecture** — Examiner la conception des crates et proposer des améliorations
- **Discussions RFC** — Proposer de nouvelles idées via les issues
- **Documentation** — Améliorer ou traduire la documentation
- **Implémentation** — Prendre en charge l'implémentation des crates une fois les conceptes stabilisés

N'hésitez pas à ouvrir une issue ou à proposer une pull request.

## 📚 Références

- [ARCHITECTURE.zh-CN.md](../ARCHITECTURE.zh-CN.md) — Démontage détaillé de l'architecture TypeScript de Claude Code
- [Documentation officielle Claude Code](https://docs.anthropic.com/en/docs/claude-code)
- [Model Context Protocol](https://modelcontextprotocol.io/)

## 📄 Licence

Ce projet est sous [licence MIT](../LICENSE).

---

<div align="center">

**Si ce projet vous est utile, pensez à lui donner une ⭐**

</div>

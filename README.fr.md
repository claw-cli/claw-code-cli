![couverture](./docs/assets/readme_cover.png)

<div align="center">

**Un agent de programmation open source, extrêmement rapide, sécurisé et indépendant des fournisseurs de modèles.**

🚧Projet en phase initiale en développement actif — pas encore prêt pour la production.
⭐ Suivez-nous en ajoutant une étoile

[![Statut](https://img.shields.io/badge/status-designing-blue?style=flat-square)](https://github.com/)
[![Langage](https://img.shields.io/badge/language-Rust-E57324?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Origine](https://img.shields.io/badge/origin-Claude_Code_TS-8A2BE2?style=flat-square)](https://docs.anthropic.com/en/docs/claude-code)
[![Licence](https://img.shields.io/badge/license-MIT-green?style=flat-square)](./LICENSE)
[![PRs Welcome](https://img.shields.io/badge/PRs-welcome-brightgreen?style=flat-square)](https://github.com/)

[English](./README.md) | [简体中文](./README.zh-CN.md) | [繁體中文](./README.zh-TW.md) | [日本語](./README.ja.md) | [한국어](./README.ko.md) | [Español](./README.es.md) | [Français](./README.fr.md) | [Português do Brasil](./README.pt-BR.md) | [Deutsch](./README.de.md) | [Русский](./README.ru.md) | [Türkçe](./README.tr.md)

<img 
  src="./docs/assets/demo_20260421.gif" 
  alt="Aperçu du projet" 
  width="100%"
  style="border-radius: 8px; box-shadow: 0 15px 40px rgba(0,0,0,0.25);object-fit:cover;"
/>

</div>

---

## 📖 Table des matières

- [Démarrage rapide](#-démarrage-rapide)
- [Installation](#-installation)
- [Questions fréquentes](#-questions-fréquentes)
- [Contribuer](#-contribuer)
- [Références](#-références)
- [Licence](#-licence)

## 📦 Installation

### Linux / macOS

```bash
curl -fsSL https://raw.githubusercontent.com/7df-lab/devo/main/install.sh | sh
```

### Windows

```powershell
irm 'https://raw.githubusercontent.com/7df-lab/devo/main/install.ps1' | iex
```

## 🚀 Démarrage rapide

Si vous préférez compiler à partir des sources, suivez les instructions ci-dessous.

### Construction

```bash
git clone https://github.com/7df-lab/devo && cd devo
cargo build --release

# linux / macos
./target/release/devo onboard

# windows
.\target\release\devo onboard
```

> [!TIP]
> Assurez-vous d'avoir Rust installé, version 1.75+ recommandée (via https://rustup.rs/).

## Questions fréquentes

### En quoi est-ce différent de Claude Code ?

C'est très similaire à Claude Code en termes de capacités. Voici les principales différences :

- 100% open source
- Non couplé à un fournisseur spécifique. Devo peut être utilisé avec Claude, OpenAI, z.ai, Qwen, Deepseek, ou même des modèles locaux. Au fur et à mesure que les modèles évoluent, les écarts entre eux se réduiront et les prix baisseront, donc être indépendant des fournisseurs est important.
- La prise en charge TUI est déjà implémentée.
- Construit avec une architecture client/serveur. Par exemple, le noyau peut s'exécuter localement sur votre machine tout en étant contrôlé à distance (par exemple, depuis une application mobile), le TUI n'étant qu'un des nombreux clients possibles.


## 🤝 Contribuer

Les contributions sont les bienvenues ! Ce projet est en phase de conception initiale, et il y a de nombreuses façons d'aider :

- **Retour sur l'architecture** — Examinez la conception des crates et suggérez des améliorations
- **Discussions RFC** — Proposez de nouvelles idées via les issues
- **Documentation** — Aidez à améliorer ou traduire la documentation
- **Implémentation** — Prenez en charge l'implémentation des crates une fois les conceptions stabilisées

N'hésitez pas à ouvrir une issue ou à soumettre une pull request.

## 📄 Licence

Ce projet est sous licence [MIT License](./LICENSE).

---

**Si vous trouvez ce projet utile, pensez à lui donner un ⭐**

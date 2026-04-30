![kapak](./docs/assets/readme_cover.png)

<div align="center">

**Son derece hızlı, güvenli ve model sağlayıcısından bağımsız bir açık kaynak kodlama aracısı.**

🚧Aktif geliştirme altında, erken aşamada bir proje — henüz üretime hazır değil.
⭐ Takip etmek için yıldız verin

[![Durum](https://img.shields.io/badge/status-designing-blue?style=flat-square)](https://github.com/)
[![Dil](https://img.shields.io/badge/language-Rust-E57324?style=flat-square&logo=rust&logoColor=white)](https://www.rust-lang.org/)
[![Köken](https://img.shields.io/badge/origin-Claude_Code_TS-8A2BE2?style=flat-square)](https://docs.anthropic.com/en/docs/claude-code)
[![Lisans](https://img.shields.io/badge/license-MIT-green?style=flat-square)](./LICENSE)
[![PR'ler Memnuniyetle](https://img.shields.io/badge/PRs-welcome-brightgreen?style=flat-square)](https://github.com/)

[English](./README.md) | [简体中文](./README.zh-CN.md) | [繁體中文](./README.zh-TW.md) | [日本語](./README.ja.md) | [한국어](./README.ko.md) | [Español](./README.es.md) | [Français](./README.fr.md) | [Português do Brasil](./README.pt-BR.md) | [Deutsch](./README.de.md) | [Русский](./README.ru.md) | [Türkçe](./README.tr.md)

<img 
  src="./docs/assets/demo_20260421.gif" 
  alt="Proje genel görünümü" 
  width="100%"
  style="border-radius: 8px; box-shadow: 0 15px 40px rgba(0,0,0,0.25);object-fit:cover;"
/>

</div>

---

## 📖 İçindekiler

- [Hızlı Başlangıç](#-hızlı-başlangıç)
- [Kurulum](#-kurulum)
- [Sıkça Sorulan Sorular](#-sıkça-sorulan-sorular)
- [Katkıda Bulunma](#-katkıda-bulunma)
- [Referanslar](#-referanslar)
- [Lisans](#-lisans)

## 📦 Kurulum

### Linux / macOS

```bash
curl -fsSL https://raw.githubusercontent.com/7df-lab/devo/main/install.sh | sh
```

### Windows

```powershell
irm 'https://raw.githubusercontent.com/7df-lab/devo/main/install.ps1' | iex
```

## 🚀 Hızlı Başlangıç

Kaynaktan derlemek isterseniz aşağıdaki talimatları kullanın.

### Derleme

```bash
git clone https://github.com/7df-lab/devo && cd devo
cargo build --release
# linux / macos
./target/release/devo onboard

# windows
.\target\release\devo onboard
```

> [!TIP]
> Rust yüklü olduğundan emin olun. 1.75+ sürümü önerilir (https://rustup.rs/ üzerinden).

## Sıkça Sorulan Sorular

### Bu, Claude Code'dan nasıl farklı?

Yetenek açısından Claude Code'a çok benzer. Başlıca farklar şunlardır:

- %100 açık kaynak
- Tek bir sağlayıcıya bağlı değildir. Devo; Claude, OpenAI, z.ai, Qwen, Deepseek veya yerel modellerle kullanılabilir. Modeller geliştikçe aradaki fark kapanacak ve fiyatlar düşecektir, bu yüzden sağlayıcıdan bağımsız olmak önemlidir.
- TUI desteği zaten uygulanmış durumda
- İstemci/sunucu mimarisiyle inşa edilmiştir. Örneğin çekirdek, makinenizde yerel olarak çalışırken uzaktan kontrol edilebilir; TUI ise mümkün olan istemcilerden yalnızca biridir

## 🤝 Katkıda Bulunma

Katkılar memnuniyetle karşılanır. Bu proje erken tasarım aşamasında ve yardımcı olmanın birçok yolu var:

- **Mimari geri bildirim** — Crate tasarımını inceleyin ve iyileştirme önerin
- **RFC tartışmaları** — Issue'lar üzerinden yeni fikirler önerin
- **Dokümantasyon** — Dokümantasyonu geliştirmeye veya çevirmeye yardımcı olun
- **Uygulama** — Tasarımlar daha sabit hale geldiğinde crate uygulamalarını üstlenin

Lütfen bir issue açmaktan veya pull request göndermekten çekinmeyin.

## 📄 Lisans

Bu proje [MIT Lisansı](./LICENSE) ile lisanslanmıştır.

---

**Bu projeyi faydalı bulursanız, bir ⭐ vermeyi düşünün**

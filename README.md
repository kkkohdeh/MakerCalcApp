<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/makercalc_wordmark_white.svg">
    <source media="(prefers-color-scheme: light)" srcset="docs/assets/makercalc_wordmark_black.svg">
    <img src="docs/assets/makercalc_wordmark_black.svg" alt="MakerCalc" width="300">
  </picture>
</p>

<p align="center">
  <strong>Tus impresoras 3D, conectadas en un solo lugar.</strong><br>
  Aplicación de escritorio para Windows que lee impresoras en la red local,<br>
  unifica sus datos y los muestra en una interfaz nativa escrita en Rust.<br>
  Integraciones actuales: Klipper/Moonraker y FlashForge.
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Rust-100%25-b7410e?style=flat-square&amp;logo=rust&amp;logoColor=white" alt="100% Rust">
  <img src="https://img.shields.io/badge/Windows-10%20%2F%2011-2563eb?style=flat-square" alt="Windows 10 y 11">
  <img src="https://img.shields.io/badge/estado-alpha-d97706?style=flat-square" alt="Estado alpha">
</p>

<p align="center">
  <a href="https://github.com/kkkohdeh/MakerCalc-Cloud#un-idioma-para-las-impresoras">Cómo funciona</a> ·
  <a href="https://github.com/kkkohdeh/MakerCalc-Cloud#ejecutar-verificaciones-en-windows">Verificaciones</a> ·
  <a href="https://github.com/kkkohdeh/MakerCalc-Cloud/blob/main/arquitectura.md">Arquitectura</a> ·
  <a href="https://github.com/kkkohdeh/MakerCalc-Cloud/blob/main/LenguajeNormalizado26-09-26.md">Contrato de telemetría</a>
</p>

---

## App y dashboard

Puedes usar la app por separado o conectarla al dashboard de [MakerCalc](https://makercalc.app). Para consultar las impresoras en vivo desde la web, mantén la app abierta y conectada en el PC del taller.

## Descargas y versiones

Consulta la sección **Releases** para los instaladores y las notas de cada versión. Revisa el estado de la versión, sus requisitos y sus limitaciones antes de instalarla.

[Descargar 0.0.2-alpha para Windows](https://github.com/kkkohdeh/MakerCalcApp/releases/download/v0.0.2-alpha/MakerCalc-Setup.exe).

En Ajustes, **Buscar actualizaciones** consulta las versiones publicadas y permite abrir el release de una versión nueva. Descarga y ejecuta su instalador; conserva tus impresoras, clave e historial. La app no descarga ni instala actualizaciones automáticamente.

## Uso local

La app necesita acceso a las impresoras en la red local. Su capacidad depende del PC y de la red; los planes y límites del dashboard web son independientes.

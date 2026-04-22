<div align="center">

[English](README.md) | Español | [简体中文](README_CN.md)

Idioma: español (Spanish)

---

# AISH

Dale al shell capacidad de pensar. Evoluciona las operaciones.

[![Official Website](https://img.shields.io/badge/Website-aishell.ai-blue.svg)](https://www.aishell.ai)
[![GitHub](https://img.shields.io/badge/GitHub-AI--Shell--Team/aish-black.svg)](https://github.com/AI-Shell-Team/aish/)
[![Python Version](https://img.shields.io/badge/python-3.10+-blue.svg)](https://www.python.org/downloads/)
[![Platform](https://img.shields.io/badge/platform-linux-lightgrey.svg)](#)
[![License](https://img.shields.io/badge/license-Apache%202.0-blue.svg)](LICENSE)

![](./docs/images/demo_show.gif)

**Un shell de IA real: PTY completo + seguridad y control de riesgos configurables**

</div>

---

## Tabla de contenidos

- [Por qué elegir AISH](#por-qué-elegir-aish)
- [Inicio rápido](#inicio-rápido)
- [Instalación](#instalación)
- [Desinstalación](#desinstalación)
- [Configuración](#configuración)
- [Uso](#uso)
- [Seguridad y control de riesgos](#seguridad-y-control-de-riesgos)
- [Skills (Plugins)](#skills-plugins)
- [Datos y privacidad](#datos-y-privacidad)
- [Documentación](#documentación)
- [Comunidad y soporte](#comunidad-y-soporte)
- [Desarrollo y pruebas](#desarrollo-y-pruebas)
- [Contribuir](#contribuir)
- [Licencia](#licencia)

---

## Por qué elegir AISH

- **Shell interactivo real**: compatibilidad PTY completa, ejecuta programas interactivos como `vim` / `ssh` / `top`
- **Integración nativa de IA**: describe tareas en lenguaje natural, genera, explica y ejecuta comandos
- **Seguro y controlable**: los comandos de IA tienen clasificación de riesgo y flujo de confirmación; preejecución en sandbox opcional para evaluar cambios
- **Extensible**: sistema de plugins Skills con carga en caliente y sobrescritura de prioridad
- **Bajo costo de migración**: compatible con comandos y flujos habituales, todo en la terminal por defecto

---

## Comparación de funciones

| Función | AISH | Claude Code |
|---------|------|-------------|
| 🎯 **Posicionamiento principal** | CLI de operaciones/solución de problemas del sistema | Asistente de desarrollo y código |
| 🤖 **Soporte multmodelo** | ✅ Totalmente abierto | ⚠️ Principalmente Claude |
| 🔧 **Sistema de subagentes** | ✅ Agente de diagnóstico ReAct | ✅ Múltiples tipos de agentes |
| 🧩 **Soporte de Skills** | ✅ Carga en caliente | ✅ |
| 🖥️ **Integración nativa con terminal** | ✅ Compatibilidad PTY completa | ⚠️ Soporte limitado |
| 🛡️ **Evaluación de riesgos de seguridad** | ✅ Confirmación de seguridad | ✅ Confirmación de seguridad |
| 🌐 **Soporte de modelos locales** | ✅ Totalmente compatible | ✅ Totalmente compatible |
| 📁 **Herramientas de operaciones de archivos** | ✅ Soporte mínimo esencial | ✅ Soporte completo |
| 💰 **Completamente gratis** | ✅ Open source | ❌ Servicio de pago |
| 📊 **Observabilidad** | ✅ Langfuse opcional | ⚠️ Integrado |
| 🌍 **Salida multilingüe** | ✅ Detección automática | ✅ |

---

## Inicio rápido

### 1) Instalar y lanzar

#### Opción 1: Instalación en una línea (recomendado)

```bash
curl -fsSL https://www.aishell.ai/repo/install.sh | bash
```

#### Opción 2: Instalación manual del bundle

Descarga el bundle correspondiente `aish-<version>-linux-<arch>.tar.gz` desde el directorio oficial de releases y luego ejecuta:

```bash
tar -xzf aish-<version>-linux-<arch>.tar.gz
cd aish-<version>-linux-<arch>
sudo ./install.sh
```

Luego inicia:

```bash
aish
```

Nota: `aish` sin subcomandos equivale a `aish run`.

### 2) Usar como un shell normal

```bash
aish> ls -la
aish> cd /etc
aish> vim hosts
```

### 3) Dejar que la IA haga el trabajo (comienza con ;)

Comenzar con `;` o `；` entra en modo IA:

```bash
aish> ;busca archivos mayores de 100M en el directorio actual y ordena por tamaño
aish> ;explica este comando: tar -czf a.tgz ./dir
```

---

## Instalación

### Bundle de release Linux

```bash
curl -fsSL https://www.aishell.ai/repo/install.sh | bash
```

El instalador resuelve la versión estable más reciente, descarga el bundle correspondiente para tu arquitectura e instala `aish`, `aish-sandbox` y `aish-uninstall` en `/usr/local/bin`.

### Ejecutar desde el código fuente (desarrollo/prueba)

```bash
uv sync
uv run aish
# o
python -m aish
```

---

## Desinstalación

Desinstalar (conservar archivos de configuración):

```bash
sudo aish-uninstall
```

Desinstalación completa (también elimina políticas de seguridad a nivel sistema):

```bash
sudo aish-uninstall --purge-config
```

Opcional: limpiar configuración del usuario (borra claves de modelo/API, etc.):

```bash
rm -rf ~/.config/aish
```

---

## Configuración

### Ubicación del archivo de configuración

- Predeterminado: `~/.config/aish/config.yaml` (o `$XDG_CONFIG_HOME/aish/config.yaml` si `XDG_CONFIG_HOME` está definido)

### Prioridad (de mayor a menor)

1. Argumentos de línea de comandos
2. Variables de entorno
3. Archivo de configuración

### Ejemplo mínimo de configuración

```yaml
# ~/.config/aish/config.yaml
model: openai/deepseek-chat
api_base: https://openrouter.ai/api/v1
api_key: your_api_key
```

Alternativamente vía variables de entorno (más adecuado para secretos):

```bash
export AISH_MODEL="openai/deepseek-chat"
export AISH_API_BASE="https://openrouter.ai/api/v1"
export AISH_API_KEY="your_api_key"

```

> Consejo: LiteLLM también admite leer variables de entorno específicas del proveedor (p. ej., `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`).

Configuración interactiva (opcional):

```bash
aish setup
```

Verificación de compatibilidad de tool calling (confirma que el modelo/canal seleccionado admite tool calling):

```bash
aish check-tool-support --model openai/deepseek-chat --api-base https://openrouter.ai/api/v1 --api-key your_api_key
```

Langfuse (observabilidad opcional):

1) Habilitar en la configuración:

```yaml
enable_langfuse: true
```

2) Configurar variables de entorno:

```bash
export LANGFUSE_PUBLIC_KEY="..."
export LANGFUSE_SECRET_KEY="..."
export LANGFUSE_HOST="https://cloud.langfuse.com"
```

`aish check-langfuse` ejecutará comprobaciones cuando `check_langfuse.py` exista en la raíz del proyecto.

---

## Uso

### Tipos de entrada comunes

| Tipo | Ejemplo | Descripción |
|:----:|---------|-------------|
| Comandos de shell | `ls -la`, `cd /path`, `git status` | Ejecutar comandos normales directamente |
| Solicitudes de IA | `;cómo comprobar el uso de puertos`, `;busca archivos mayores de 100M` | Entrar en modo IA con el prefijo `;`/`；` |
| Comandos integrados | `help`, `clear`, `exit`, `quit` | Comandos de control integrados del shell |
| Cambio de modelo | `/model gpt-4` | Ver o cambiar modelo |

### Compatibilidad de shell (PTY)

```bash
aish> ssh user@host
aish> top
aish> vim /etc/hosts
```

---

## Seguridad y control de riesgos

AI Shell solo realiza la evaluación de seguridad sobre comandos **generados por IA y listos para ejecutarse**.

### Niveles de riesgo

- **LOW**: Permitido por defecto
- **MEDIUM**: Confirmación antes de ejecutar
- **HIGH**: Bloqueado por defecto

### Ruta del archivo de políticas de seguridad

Los archivos de políticas se resuelven en este orden:
1. `/etc/aish/security_policy.yaml` (nivel sistema)
2. `~/.config/aish/security_policy.yaml` (nivel usuario; plantilla generada automáticamente si no existe)

### Pre-ejecución en sandbox (opcional, recomendada en producción)

La política predeterminada tiene la pre‑ejecución en sandbox **deshabilitada**. Para habilitarla:

1) Configurar en la política de seguridad:

```yaml
global:
  enable_sandbox: true
```

2) Iniciar el servicio de sandbox privilegiado (systemd):

```bash
sudo systemctl enable --now aish-sandbox.socket
```

Socket predeterminado: `/run/aish/sandbox.sock`.
Cuando el sandbox no está disponible, se aplica la regla `sandbox_off_action` (BLOCK/CONFIRM/ALLOW) de la política.

---

## Skills (Plugins)

Los Skills amplían el conocimiento y los flujos de trabajo de la IA, con carga en caliente y sobrescritura de prioridad.

Directorios de escaneo predeterminados y prioridad:
- `~/.config/aish/skills/` (o `$AISH_CONFIG_DIR/skills`)
- `~/.claude/skills/`

Las versiones empaquetadas intentan copiar las skills a nivel sistema en el directorio del usuario en el primer inicio (p. ej., `/usr/share/aish/skills`).

Para más detalles: `docs/skills-guide.md`

---

## Datos y privacidad

Este proyecto almacena los siguientes datos localmente (para diagnóstico y trazabilidad):

- **Logs**: predeterminado `~/.config/aish/logs/aish.log`
- **Sesiones/Historial**: predeterminado `~/.local/share/aish/sessions.db` (SQLite)
- **Descarga de salida grande**: predeterminado `~/.local/share/aish/offload/`

Recomendaciones:
- No cometas claves API reales en el repositorio; prefiere variables de entorno o sistemas de gestión de secretos.
- En producción, combina políticas de seguridad para limitar el alcance de directorios accesibles por la IA.

---

## Documentación

- Guía de configuración: `CONFIGURATION.md`
- Inicio rápido: `QUICKSTART.md`
- Uso de Skills: `docs/skills-guide.md`
- Mecanismo de corrección de comandos: `docs/command-interaction-correction.md`

---

## Comunidad y soporte

| Enlace | Descripción |
|------|-------------|
| [Official Website](https://www.aishell.ai) | Página del proyecto y más información |
| [GitHub Repository](https://github.com/AI-Shell-Team/aish/) | Código fuente y seguimiento de issues |
| [GitHub Issues](https://github.com/AI-Shell-Team/aish/issues) | Reportes de errores |
| [GitHub Discussions](https://github.com/AI-Shell-Team/aish/discussions) | Debates de la comunidad |
| [Discord](https://discord.com/invite/Pw2mjZt3) | Unirse a la comunidad |

---

## Desarrollo y pruebas

```bash
uv sync
uv run aish
uv run pytest
```

---

## Contribuir

Consulta [CONTRIBUTING.md](CONTRIBUTING.md) para las directrices.
---

## Licencia

`LICENSE` (Apache 2.0)

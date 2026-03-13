Idioma: español (Spanish)

# Hoja de ruta de AISH

> **Visión**: Transformar AISH de un shell de IA intrusivo en un asistente sidecar inteligente que otorgue a los usuarios control absoluto sobre su línea de comandos.

---

## 🎯 Objetivos estratégicos (Q2 2026)

### Principios clave
1. **Control del usuario primero**: la IA ofrece sugerencias, nunca bloquea el flujo principal
2. **Asíncrono por defecto**: todo el análisis de IA ocurre en segundo plano
3. **Intervención inteligente**: reducir falsos positivos en 95 % mediante filtrado inteligente
4. **Mejora progresiva**: mantener compatibilidad hacia atrás mientras evoluciona la arquitectura

### Métricas de éxito
- ✅ La ejecución de comandos vuelve al prompt en < 50 ms
- ✅ Cero llamadas de IA bloqueantes en la ruta principal de ejecución
- ✅ Tasa de falsos positivos < 5 %
- ✅ Satisfacción de usuarios > 4,5/5

---

## 📅 Línea de tiempo de releases

```
Semana 1-2   │ v0.1.0 → v0.2.0  │ Arquitectura sidecar de IA asíncrona
Semana 3-4   │ v0.2.0 → v0.3.0  │ Análisis inteligente y controles de usuario
Semana 5-6   │ v0.3.0 → v0.4.0  │ Modo Plan y herramientas mejoradas
Semana 7-8   │ v0.4.0 → v0.5.0  │ Sistema multi‑agente
Semana 9-10  │ v0.5.0 → v0.6.0  │ Rich UI y gestión de tareas
Semana 11-12 │ v0.6.0 → v0.7.0  │ Agent SDK y protocolo MCP
```

---

## 🚀 Fase 1: Revisión de arquitectura (Semanas 1-4)

### v0.1.1 (Semana 1) - Correcciones críticas
**Tipo**: Release de parche

**Correcciones**
- 🐛 Implementar la invocación de la herramienta skill (`skill.py` TODO)
- 🐛 Corregir el historial de tool calls no añadido a la memoria (`shell.py:889`)
- 🐛 Deshabilitar el auto‑disparo de `handle_error_detect()` (mitigación temporal)

**Impacto**: Reduce puntos de dolor inmediatos de usuarios

---

### v0.2.0 (Semana 2) - Sidecar de IA asíncrono 🔥
**Tipo**: Release menor (Breaking Changes)

**Nueva arquitectura**
```
┌─────────────────────────────────────────────────────────┐
│                 Proceso principal del shell             │
│  ┌──────────────┐                                       │
│  │   Entrada    │ → Ejecutar → Mostrar → Prompt         │
│  └──────────────┘      ↓                                │
│               Encolar evento (no bloqueante)            │
└─────────────────────────┼───────────────────────────────┘
                          ↓
┌─────────────────────────────────────────────────────────┐
│               Sidecar de IA en segundo plano            │
│  ┌──────────────┐   ┌──────────────┐   ┌────────────┐ │
│  │ Cola eventos │ → │ Filtro smart │ → │ LLM Worker │ │
│  └──────────────┘   └──────────────┘   └────────────┘ │
│                          ↓                               │
│                   ┌──────────────┐                      │
│                   │ Notificación │ → Indicador [AI:2]  │
│                   └──────────────┘                      │
└─────────────────────────────────────────────────────────┘
```

**Nuevos módulos**
- `src/aish/sidecar/event_queue.py` - Cola de eventos de comandos no bloqueante
- `src/aish/sidecar/analyzer.py` - Lógica de filtrado inteligente
- `src/aish/sidecar/worker.py` - Worker de análisis LLM en segundo plano
- `src/aish/sidecar/notification.py` - Sistema de notificación no intrusivo
- `src/aish/sidecar/storage.py` - Persistencia de resultados de análisis

**Características clave**
1. **Cola de eventos**: los eventos de finalización de comandos se encolan sin bloquear el flujo principal
2. **Analizador inteligente**: filtra falsos positivos (grep, diff, ssh, Ctrl‑C, etc.)
3. **Worker en segundo plano**: análisis LLM asíncrono en una tarea separada
4. **Centro de notificaciones**: indicador de estado ligero en el prompt (`[AI:2]`)

**Comandos de usuario**
- `:ai` o `:ai last` - Ver la sugerencia más reciente
- `:ai list` - Listar todas las sugerencias pendientes
- `:ai show <n>` - Ver una sugerencia específica
- `:ai apply <n>` - Aplicar sugerencia (entra al flujo de aprobación de seguridad)

**Breaking Changes**
- ❌ Eliminado el auto‑disparo de `handle_error_detect()`
- ❌ Eliminado el auto‑disparo de `handle_command_error()`
- ❌ Deprecado `ShellState.CORRECT_PENDING`

**Guía de migración**
- Comportamiento antiguo: la IA interrumpía automáticamente en fallos de comandos
- Nuevo comportamiento: la IA analiza en segundo plano, el usuario consulta sugerencias explícitamente
- Compatibilidad: se conserva la API `handle_command_error()` para invocación manual

**Configuración**
```yaml
# ~/.config/aish/config.yaml
sidecar:
  enabled: true
  max_queue_size: 100
  worker_threads: 1
```

---

### v0.2.1 (Semana 3) - Estabilidad
**Tipo**: Release de parche

**Correcciones**
- 🐛 Corregir fuga de memoria del worker sidecar
- 🐛 Optimizar el rendimiento de la cola de eventos
- 📊 Agregar métricas de análisis del sidecar (tasa de éxito, latencia)

---

### v0.3.0 (Semana 4) - Análisis inteligente
**Tipo**: Release menor

**Inteligencia mejorada**
- 🧠 **Filtrado consciente del contexto**
  - Análisis del historial de comandos (fallos consecutivos → mayor prioridad)
  - Seguimiento del comportamiento del usuario (reintento inmediato → menor prioridad)
  - Ajuste basado en el tiempo (tarde en la noche → menor prioridad)

**Estrategias configurables**
```yaml
sidecar:
  analysis_mode: smart  # smart | aggressive | minimal
  notification_style: indicator  # indicator | toast | silent

  # Reglas del modo smart
  smart_rules:
    ignore_commands: [grep, diff, test, ssh]
    ignore_exit_codes: [130]  # Ctrl-C
    benign_stderr_patterns:
      - "^Warning:"
      - "^Note:"
```

**Nuevos comandos**
- `:ai clear` - Limpiar todas las sugerencias
- `:ai stats` - Mostrar estadísticas de análisis

**Mejoras**
- 95 % de reducción de falsos positivos
- < 50 ms de sobrecarga de ejecución de comandos
- Estilos de notificación configurables

---

## 🛠️ Fase 2: Capacidades centrales (Semanas 5-8)

### v0.3.1 (Semana 5) - Refinamiento
**Tipo**: Release de parche

**Correcciones**
- 🐛 Corregir casos límite en el filtrado inteligente
- 🐛 Optimizar el rendimiento del almacenamiento de sugerencias

---

### v0.4.0 (Semana 6) - Modo Plan y herramientas
**Tipo**: Release menor

**Modo Plan** (inspirado en Claude Code)
- 🎯 `PlanAgent`: experto en descomposición de tareas
- 📋 Flujo de aprobación del usuario antes de ejecutar
- 💾 Persistencia del plan en `.aish/plans/`

**Uso**
```bash
aish> ;deploy the application to production
[La IA crea un plan]
┌─ Plan de despliegue ───────────────────────────────────┐
│ 1. Ejecutar la suite de pruebas                         │
│ 2. Construir la imagen Docker                           │
│ 3. Subir al registro                                    │
│ 4. Actualizar el despliegue de Kubernetes               │
│                                                      │
│ ¿Aprobar? [y/N]:                                        │
└───────────────────────────────────────────────────────┘
```

**Suite de herramientas mejorada**
- 🔍 `WebSearchTool`: integración con DuckDuckGo
- 🔧 `GitTool`: wrapper de operaciones Git (status, diff, commit, push)
- 🧠 `CodeAnalysisTool`: análisis de código basado en AST (tree‑sitter)

**Nuevos comandos**
- `:plan` - Entrar en modo plan
- `:plan show` - Ver el plan actual
- `:plan approve` - Aprobar y ejecutar el plan

---

### v0.4.1 (Semana 7) - Pulido
**Tipo**: Release de parche

**Correcciones**
- 🐛 Corregir casos límite del modo plan
- 🐛 Optimizar el parseo de resultados de WebSearch
- 📝 Agregar documentación del modo plan

---

### v0.5.0 (Semana 8) - Sistema multi‑agente
**Tipo**: Release menor

**Ecosistema de agentes**
- 🤖 `CodeReviewAgent`: análisis estático + mejores prácticas
- 🐛 `DebugAgent`: análisis de logs + identificación de causa raíz
- 🔍 `ResearchAgent`: búsqueda web + documentación
- 🎭 `AgentOrchestrator`: coordinación de agentes en paralelo/secuencial

**Arquitectura de agentes**
```python
# Ejemplo: ejecución paralela de agentes
aish> ;review this PR and check for security issues

[Agentes lanzados en paralelo]
├─ CodeReviewAgent: Analizando calidad de código...
├─ SecurityAgent: Escaneando vulnerabilidades...
└─ TestAgent: Comprobando cobertura de pruebas...

[Resultados agregados y presentados]
```

**Gestión inteligente del contexto**
- 📊 Clasificación de mensajes basada en prioridad
- 🗜️ Auto‑resumen de conversaciones largas (usando modelo pequeño)
- 🧠 Base de conocimiento entre sesiones (búsqueda vectorial opcional)

**Configuración**
```yaml
agents:
  enabled: true
  max_parallel: 3
  context_window: 8000
  auto_summarize: true
```

---

## 🎨 Fase 3: Experiencia de usuario y ecosistema (Semanas 9-12)

### v0.5.1 (Semana 9) - Estabilidad
**Tipo**: Release de parche

**Correcciones**
- 🐛 Corregir condiciones de carrera en ejecución paralela de agentes
- 🐛 Optimizar el algoritmo de compresión de contexto
- 📊 Agregar métricas de rendimiento de agentes

---

### v0.6.0 (Semana 10) - Rich UI y tareas
**Tipo**: Release menor

**Mejoras de Rich UI**
- 🎨 Barras de progreso en tiempo real para ejecución de agentes
- 🌳 Visualización del árbol de tareas
- 🔍 Paneles de confirmación interactivos con vista previa de diffs

**Sistema de gestión de tareas** (inspirado en Claude Code)
- 📋 Seguimiento de tareas integrado (`TaskCreate`, `TaskUpdate`, `TaskList`)
- 🔗 Dependencias y prioridades de tareas
- 💾 Persistencia y recuperación de tareas

**Uso**
```bash
aish> :task list
┌─ Tareas activas ───────────────────────────────────────┐
│ [1] ⏳ Implementar autenticación de usuario             │
│     ├─ [2] ✅ Configurar el esquema de base de datos     │
│     ├─ [3] 🔄 Crear endpoint de login                   │
│     └─ [4] ⏸️  Agregar validación de token JWT          │
└───────────────────────────────────────────────────────┘

aish> :task show 3
[Vista detallada de la tarea con progreso y bloqueos]
```

**Nuevos comandos**
- `:task create` - Crear nueva tarea
- `:task list` - Listar todas las tareas
- `:task show <id>` - Ver detalles de la tarea
- `:task complete <id>` - Marcar tarea como completada

---

### v0.6.1 (Semana 11) - Optimización
**Tipo**: Release de parche

**Correcciones**
- 🐛 Corregir uso de memoria del gestor de tareas
- 🐛 Optimizar el rendimiento de renderizado de Rich UI
- 📝 Agregar documentación de gestión de tareas

---

### v0.7.0 (Semana 12) - Agent SDK y MCP
**Tipo**: Release menor

**Agent SDK**
- 🔌 Interfaz estandarizada para desarrollo de agentes
- 🏗️ Generador de plantillas de agentes (`aish create-agent`)
- 🌐 Marketplace de agentes (compartido por la comunidad)

**Ejemplo de desarrollo de agentes**
```bash
# Crear un nuevo agente
aish create-agent --name my-agent --type diagnostic

# Estructura generada
~/.config/aish/agents/my-agent/
├── agent.py          # Implementación del agente
├── config.yaml       # Configuración del agente
├── README.md         # Documentación
└── tests/            # Pruebas unitarias
```

**Soporte de protocolo MCP**
- 🔗 Compatible con servidores MCP de Claude Desktop
- 🔌 Cliente MCP integrado para servicios externos
- 📡 Comunicación bidireccional con el ecosistema MCP

**Panel de observabilidad** (opcional)
- 📊 Web UI (basada en FastAPI)
- 📈 Monitoreo de sesiones en tiempo real
- 💰 Estadísticas de uso de tokens
- ⚡ Analítica de rendimiento de agentes

**Configuración**
```yaml
mcp:
  enabled: true
  servers:
    - name: filesystem
      command: npx
      args: [-y, @modelcontextprotocol/server-filesystem, /tmp]
    - name: github
      command: npx
      args: [-y, @modelcontextprotocol/server-github]
      env:
        GITHUB_TOKEN: ${GITHUB_TOKEN}

observability:
  enabled: false  # Panel web opcional
  port: 8080
```

---

## 📊 Matriz de comparación de funciones

| Función | v0.1.0 (Actual) | v0.2.0 | v0.4.0 | v0.6.0 | v0.7.0 |
|---------|------------------|--------|--------|--------|--------|
| **Análisis de IA asíncrono** | ❌ | ✅ | ✅ | ✅ | ✅ |
| **Filtrado inteligente** | ❌ | ⚠️ Básico | ✅ Avanzado | ✅ | ✅ |
| **Modo Plan** | ❌ | ❌ | ✅ | ✅ | ✅ |
| **Multi‑agente** | ⚠️ 1 agente | ⚠️ 1 agente | ⚠️ 1 agente | ✅ 4+ agentes | ✅ |
| **Gestión de tareas** | ❌ | ❌ | ❌ | ✅ | ✅ |
| **Agent SDK** | ❌ | ❌ | ❌ | ❌ | ✅ |
| **Protocolo MCP** | ❌ | ❌ | ❌ | ❌ | ✅ |
| **Rich UI** | ⚠️ Básico | ⚠️ Básico | ⚠️ Básico | ✅ | ✅ |

---

## 🎯 Posicionamiento competitivo

### vs Claude Code

| Aspecto | AISH v0.7.0 | Claude Code |
|--------|-------------|-------------|
| **Open Source** | ✅ Apache 2.0 | ❌ Propietario |
| **Modelos locales** | ✅ Soporte completo | ❌ Limitado |
| **Multi‑proveedor** | ✅ LiteLLM | ⚠️ Principalmente Claude |
| **Soporte PTY** | ✅ Completo | ⚠️ Limitado |
| **Enfoque Ops** | ✅ Diagnóstico de sistemas | ⚠️ Enfoque dev |
| **Privacidad** | ✅ Local‑first | ⚠️ Basado en la nube |
| **Costo** | ✅ Gratis | ❌ Suscripción |
| **IA asíncrona** | ✅ No bloqueante | ⚠️ Bloqueante |

### Propuestas de valor únicas

1. **IA no intrusiva**: el análisis en segundo plano nunca bloquea el flujo de trabajo del usuario
2. **Ops‑nativo**: construido para administración de sistemas y resolución de problemas
3. **Privacidad primero**: soporte de modelos locales sin salida de datos
4. **Impulsado por la comunidad**: open source con ecosistema de agentes extensible
5. **Listo para la empresa**: sandbox, logs de auditoría y permisos finos

---

## 🚨 Gestión de riesgos

| Riesgo | Impacto | Mitigación | Estado |
|------|--------|------------|--------|
| **Complejidad async** | Alto | Pruebas extensivas + modo fallback | Semanas 1-2 |
| **Precisión del smart filter** | Medio | Reglas configurables + feedback de usuarios | Semanas 3-4 |
| **Uso de recursos del worker** | Medio | Límites de cola + auto‑throttling | Semanas 2-3 |
| **Adopción de usuarios** | Bajo | Migración progresiva + docs | En curso |
| **Coordinación de agentes** | Medio | Límites de tiempo + recuperación de errores | Semanas 7-8 |

---

## 📈 Métricas de éxito

### KPI técnicos
- ✅ Latencia de ejecución de comandos < 50 ms (P95)
- ✅ Tasa de falsos positivos de análisis de IA < 5 %
- ✅ Memoria del worker sidecar < 50 MB
- ✅ Tiempo de respuesta de agentes < 2 s (P95)
- ✅ Cobertura de pruebas > 80 %

### KPI de usuarios
- 📈 Crecimiento de usuarios activos diarios > 20 % MoM
- ⭐ Satisfacción de usuarios > 4,5/5
- 🤝 Skills de la comunidad > 50
- 🏢 Despliegues empresariales > 10

### KPI del ecosistema
- 🔌 Agentes de la comunidad > 20
- 📦 Integraciones MCP > 5
- 📝 Completitud de la documentación > 90 %

---

## 🤝 Contribuir

¡Damos la bienvenida a contribuciones! Áreas prioritarias:

### Semanas 1-4 (Fase 1)
- 🔧 Implementación de la arquitectura sidecar
- 🧪 Desarrollo de reglas de filtrado inteligente
- 📝 Documentación de la guía de migración

### Semanas 5-8 (Fase 2)
- 🤖 Nuevas implementaciones de agentes
- 🔍 Integraciones de herramientas (búsqueda web, análisis de código)
- 🧠 Optimización de la gestión de contexto

### Semanas 9-12 (Fase 3)
- 🎨 Mejoras de UI/UX
- 🔌 Desarrollo del Agent SDK
- 📊 Panel de observabilidad

Consulta [CONTRIBUTING.md](CONTRIBUTING.md) para directrices detalladas.

---

## 📚 Recursos

- **Documentación**: [docs.aishell.ai](https://docs.aishell.ai)
- **GitHub**: [github.com/AI-Shell-Team/aish](https://github.com/AI-Shell-Team/aish)
- **Discord**: [discord.gg/aish](https://discord.gg/aish)

---

## 📝 Changelog

### Próximamente
- Ver las secciones de release anteriores

### v0.1.0 (Actual)
- ✅ Soporte PTY completo
- ✅ Soporte multi‑modelo (LiteLLM)
- ✅ Evaluación básica de riesgos de seguridad
- ✅ Sistema de hot‑reload de Skills
- ✅ Agente de diagnóstico ReAct
- ✅ Persistencia de sesiones (SQLite)
- ✅ Mecanismo de offload de salida
- ✅ Soporte i18n

---

**Última actualización**: 2026-03-06
**Versión de la hoja de ruta**: 2.0
**Estado**: 🟢 Desarrollo activo

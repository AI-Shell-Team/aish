---
name: aish-add-skills
description: Create new skills from documents, tutorials, or examples. Use when user wants to create a skill from learning materials or existing content.
---

# AISH Create Skills

Create reusable skills by extracting methodologies from learning materials or examples.

## Quick Example

**User**: "根据这篇 Docker 教程创建一个 skill"

**AI**: 读取文档 → 提取步骤 → 生成 skill 结构 → 保存到 `~/.config/aish/skills/`

---

# Create Mode Overview

Extract actionable methodologies from learning materials or examples to generate reusable Skills.

## When to Use

Use Create Mode when:
- User provides a document/article/tutorial
- User provides an example to learn from
- User says "根据这篇文章创建一个 skill"
- User says "从这个示例学习并创建 skill"

---

## Step 0: Identify Input Type

**Critical first step** - determine which processing path:

```
User Input
    │
    ├─ Has teaching intent? ("how to", "steps", "guide")
    │   └─ YES → Path A: Methodology Document
    │
    ├─ Is a finished work? (article, design, code, proposal)
    │   └─ YES → Path B: Example (Reverse Engineering)
    │
    └─ Neither? → Tell user this content is not suitable
```

### Path A indicators (Methodology Document):
- Contains "how to", "steps", "method", "guide"
- Has numbered lists or step sequences
- Written with teaching intent

### Path B indicators (Example/Output):
- Is a complete work/artifact
- No teaching intent
- Is "the thing itself" rather than "how to make the thing"

---

## Path A: Extract from Methodology Document

### A1: Validate Document Suitability

Check if suitable for skill generation (must meet at least 2):
- [ ] Has clear goal/outcome
- [ ] Has repeatable steps/process
- [ ] Has quality criteria
- [ ] Has context/scenario description

**If not suitable**: Tell user honestly and explain why.

### A2: Identify Skill Type

| Type | Characteristics | Examples |
|------|-----------------|----------|
| **How-to** | Clear step sequence, input→output | Deploy Docker, Configure CI/CD |
| **Decision** | Conditions, trade-offs, choices | Choose database, Select framework |
| **Framework** | Mental model, analysis dimensions | SWOT, 5W1H, First Principles |
| **Checklist** | Verification list, pass/fail criteria | Code review checklist, Launch checklist |

### A3: Extract Structure by Type

**For How-to:** Prerequisites → Step sequence → Expected result → Common errors

**For Decision:** Decision factors → Options with pros/cons → Decision tree → Recommended default

**For Framework:** Core concepts → Analysis dimensions → Application method → Limitations

**For Checklist:** Check items with criteria → Priority levels → Commonly missed items

### A4: Generate Skill Directory

**File naming principles:**
- Use descriptive names that reflect actual content
- Avoid generic names like `template.md` or `sample.md`

**Create structure based on skill needs:**

```
{skill-name}/
├── SKILL.md                      # Main instructions (required)
├── {descriptive-name}.md         # Reference docs (name by content)
├── examples/                     # Example outputs (descriptive names)
│   └── {example-description}.md
└── scripts/                      # Helper scripts (name by purpose)
    └── {script-purpose}.{py|sh}
```

**Example for blog-post-writer skill:**
```
blog-post-writer/
├── SKILL.md
├── structure-template.md   # Blog post structure template
├── examples/
│   ├── technical-post.md   # Example technical blog post
│   └── tutorial-post.md    # Example tutorial post
└── scripts/
    └── word-count.py       # Word counter script
```

### A5: Write SKILL.md

Use this template:

```markdown
---
name: {skill-name}
description: {What it does and when to use it}
---

# {Skill Name}

{Brief description}

## When to Use

Use this skill when:
- {Scenario 1}
- {Scenario 2}

## Prerequisites

- {Prerequisite 1}
- {Prerequisite 2}

## Steps

1. **{Step 1}** - {Expected outcome}
   - {Detail}

2. **{Step 2}** - {Expected outcome}
   - {Detail}

## Quality Checkpoints

- [ ] {Checkpoint 1}
- [ ] {Checkpoint 2}

## Common Pitfalls

- **{Pitfall 1}**: {How to avoid}

## Source

- Document: {name/URL}
- Extracted: {timestamp}
```

**Reference supporting files:**
```markdown
## Additional Resources

- For detailed template, see [structure-template.md](structure-template.md)
- For examples, see [examples/](examples/)
```

### A6: Save to Disk

```bash
SKILL_DIR=~/.config/aish/skills/{skill-name}

# Create directories only if needed
# mkdir -p "$SKILL_DIR/examples"
# mkdir -p "$SKILL_DIR/scripts"

# Write SKILL.md (required)
cat > "$SKILL_DIR/SKILL.md" << 'EOF'
{SKILL.md content}
EOF

# Write optional files with descriptive names
```

---

## Path B: Reverse Engineer from Example

### B1: Identify Output Type

What kind of artifact is this?
- Technical blog post
- Product proposal/PRD
- Academic paper
- Code architecture
- Design document
- Other: [specify]

### B2: Analyze Structure

```
Structure Analysis:
├── [Part 1]: [Function] - [Proportion %]
├── [Part 2]: [Function] - [Proportion %]
└── [Part N]: [Function] - [Proportion %]
```

Questions:
- How many parts?
- What's the function of each part?
- What's the order and proportion?

### B3: Extract Quality Characteristics

| Dimension | Questions |
|-----------|-----------|
| Structure | How is content organized? |
| Style | Tone, word choice, expression? |
| Technique | What methods make it effective? |
| Logic | How does information flow? |
| Details | Small but important touches? |

### B4: Reverse Engineer Process

```markdown
## Deduced Production Steps
1. [Step 1]: [What to do] - [Key point]
2. [Step 2]: [What to do] - [Key point]

## Key Decisions
- [Decision 1]: [Options] - [Chose X because...]

## Reusable Techniques
- [Technique 1]: [How to apply]
```

### B5: Generate Skill Directory

**Example for technical-blog-writer skill:**
```
technical-blog-writer/
├── SKILL.md
├── blog-post-template.md   # Template for blog posts
├── examples/
│   └── react-hooks-post.md # The analyzed example
└── scripts/
    └── readability.py      # Readability checker
```

### B6: Write SKILL.md

```markdown
---
name: {skill-name}
description: {What it does}
---

# {Skill Name}

{Brief description}

## Output Type

{What kind of artifact this produces}

## When to Use

Use this skill when:
- {Scenario 1}
- {Scenario 2}

## Structure Template

1. [Part 1]: [Function] - [~X%]
2. [Part 2]: [Function] - [~X%]

## Quality Characteristics

Learned from example:
- [Characteristic 1]: [How it manifests]
- [Characteristic 2]: [How it manifests]

## Production Steps

1. **{Step 1}**: [What to do] - [Tips]
2. **{Step 2}**: [What to do] - [Tips]

## Checklist

- [ ] [Check item 1]
- [ ] [Check item 2]

## Reference Example

- Source: [name/URL]
- Analyzed: [timestamp]

## Additional Resources

- For template, see [blog-post-template.md](blog-post-template.md)
- For reference example, see [examples/react-hooks-post.md](examples/react-hooks-post.md)
```

### B7: Save to Disk

```bash
SKILL_DIR=~/.config/aish/skills/{skill-name}

# Write SKILL.md
cat > "$SKILL_DIR/SKILL.md" << 'EOF'
{SKILL.md content}
EOF

# Write optional files with descriptive names
```

---

## Skill Best Practices

### File Naming

**Good naming:**
- `api-reference.md` (not `reference.md`)
- `blog-post-template.md` (not `template.md`)
- `examples/react-hooks-post.md` (not `examples/sample.md`)
- `scripts/validate-readability.py` (not `scripts/helper.py`)

**Avoid generic names** like: `template.md`, `sample.md`, `original.md`, `helper.py`

### SKILL.md Guidelines

- Keep **under 500 lines**
- Focus on essentials in SKILL.md
- Move detailed reference to separate files
- Reference supporting files with markdown links

### Frontmatter (Optional)

```yaml
---
name: my-skill
description: What this skill does and when to use it
disable-model-invocation: true  # Only user can invoke
user-invocable: false            # Hide from / menu
allowed-tools: Read, Grep        # Restrict tool access
argument-hint: [filename]        # Autocomplete hint
---
```

### Types of Skills

**Reference content**: Knowledge, conventions, patterns (runs inline)
- Example: API conventions, coding standards

**Task content**: Step-by-step instructions (often user-invoked)
- Example: Deploy, commit, code generation

---

## Installation Path

Skills are saved to: `~/.config/aish/skills/`

aish automatically hot-reloads skills from this directory.

---

## Important Notes

1. Always validate content suitability before extracting
2. Take time to truly understand the content
3. Preserve sources - always credit where knowledge came from
4. Use descriptive file names that reflect actual content
5. Keep SKILL.md under 500 lines - move detailed reference to supporting files

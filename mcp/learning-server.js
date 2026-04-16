#!/usr/bin/env node
const readline = require('readline');
const fs = require('fs');
const path = require('path');
const os = require('os');

const LEARNING_DIR = path.join(os.homedir(), '.pulse', 'learning');
const MISTAKES_FILE = path.join(LEARNING_DIR, 'mistakes.json');
const RULES_FILE = path.join(LEARNING_DIR, 'rules.json');

function ensureDir(dir) {
  fs.mkdirSync(dir, { recursive: true });
}

function readJSON(filePath, fallback) {
  try {
    return JSON.parse(fs.readFileSync(filePath, 'utf8'));
  } catch (_) {
    return fallback;
  }
}

function writeJSON(filePath, data) {
  ensureDir(path.dirname(filePath));
  fs.writeFileSync(filePath, JSON.stringify(data, null, 2));
}

function respond(id, result) {
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id, result }) + '\n');
}

function respondError(id, code, message) {
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id, error: { code, message } }) + '\n');
}

function learnMistake(description, context, correction) {
  const mistakes = readJSON(MISTAKES_FILE, []);
  const entry = {
    id: mistakes.length + 1,
    description,
    context,
    correction,
    timestamp: new Date().toISOString()
  };
  mistakes.push(entry);
  writeJSON(MISTAKES_FILE, mistakes);
  return { logged: true, id: entry.id, total_mistakes: mistakes.length };
}

function learnGetRules() {
  const rules = readJSON(RULES_FILE, []);
  return { count: rules.length, rules };
}

function learnAddRule(rule, reason) {
  const rules = readJSON(RULES_FILE, []);
  const entry = {
    id: rules.length + 1,
    rule,
    reason,
    created_at: new Date().toISOString()
  };
  rules.push(entry);
  writeJSON(RULES_FILE, rules);
  return { added: true, id: entry.id, total_rules: rules.length };
}

function learnGetMistakes(limit) {
  const mistakes = readJSON(MISTAKES_FILE, []);
  const n = limit || 10;
  const recent = mistakes.slice(-n).reverse();
  return { count: recent.length, total: mistakes.length, mistakes: recent };
}

function learnAnalyze() {
  const mistakes = readJSON(MISTAKES_FILE, []);
  if (mistakes.length === 0) {
    return { total: 0, summary: 'No mistakes logged yet.', categories: {} };
  }

  // Extract keywords from descriptions to group by category
  const keywords = {};
  const commonWords = new Set(['the', 'a', 'an', 'is', 'was', 'to', 'in', 'for', 'of', 'and', 'with', 'on', 'it', 'that', 'this', 'not', 'but', 'from', 'or', 'be', 'at', 'by', 'i']);

  for (const m of mistakes) {
    const words = m.description.toLowerCase().replace(/[^a-z0-9\s-]/g, '').split(/\s+/);
    for (const word of words) {
      if (word.length > 2 && !commonWords.has(word)) {
        keywords[word] = (keywords[word] || 0) + 1;
      }
    }
  }

  // Get top categories (words appearing more than once)
  const categories = {};
  for (const [word, count] of Object.entries(keywords)) {
    if (count >= 2) {
      categories[word] = count;
    }
  }

  // Sort categories by frequency
  const sorted = Object.entries(categories).sort((a, b) => b[1] - a[1]).slice(0, 10);

  // Build summary
  const parts = sorted.map(([word, count]) => `${count} related to "${word}"`);
  const summary = mistakes.length === 1
    ? `1 mistake logged.`
    : `${mistakes.length} mistakes logged. Patterns: ${parts.length > 0 ? parts.join(', ') : 'no clear patterns yet (each mistake is unique)'}.`;

  return {
    total: mistakes.length,
    summary,
    top_categories: Object.fromEntries(sorted),
    oldest: mistakes[0].timestamp,
    newest: mistakes[mistakes.length - 1].timestamp
  };
}

const TOOLS = [
  {
    name: 'learn_mistake',
    description: 'Log a mistake: what went wrong, context, and the correct action',
    inputSchema: {
      type: 'object',
      properties: {
        description: { type: 'string', description: 'What went wrong' },
        context: { type: 'string', description: 'Context when the mistake happened' },
        correction: { type: 'string', description: 'What the correct action should have been' }
      },
      required: ['description', 'context', 'correction']
    }
  },
  {
    name: 'learn_get_rules',
    description: 'Get all learned rules the agent should follow',
    inputSchema: { type: 'object', properties: {} }
  },
  {
    name: 'learn_add_rule',
    description: 'Add a new rule for the agent to follow',
    inputSchema: {
      type: 'object',
      properties: {
        rule: { type: 'string', description: 'The rule to follow' },
        reason: { type: 'string', description: 'Why this rule exists' }
      },
      required: ['rule', 'reason']
    }
  },
  {
    name: 'learn_get_mistakes',
    description: 'Get recent mistakes for review (default last 10)',
    inputSchema: {
      type: 'object',
      properties: {
        limit: { type: 'number', description: 'Number of recent mistakes to return (default 10)' }
      }
    }
  },
  {
    name: 'learn_analyze',
    description: 'Analyze patterns in logged mistakes and return a summary',
    inputSchema: { type: 'object', properties: {} }
  }
];

function handleToolCall(name, args) {
  switch (name) {
    case 'learn_mistake':
      return learnMistake(args.description, args.context, args.correction);
    case 'learn_get_rules':
      return learnGetRules();
    case 'learn_add_rule':
      return learnAddRule(args.rule, args.reason);
    case 'learn_get_mistakes':
      return learnGetMistakes(args.limit);
    case 'learn_analyze':
      return learnAnalyze();
    default:
      return { error: `Unknown tool: ${name}` };
  }
}

const rl = readline.createInterface({ input: process.stdin });

rl.on('line', (line) => {
  let msg;
  try {
    msg = JSON.parse(line);
  } catch (e) {
    return;
  }

  const { id, method, params } = msg;

  if (method === 'initialize') {
    respond(id, {
      protocolVersion: '2024-11-05',
      capabilities: { tools: {} },
      serverInfo: { name: 'pulse-learning', version: '1.0.0' }
    });
    return;
  }

  if (method === 'notifications/initialized') {
    return;
  }

  if (method === 'tools/list') {
    respond(id, { tools: TOOLS });
    return;
  }

  if (method === 'tools/call') {
    const { name, arguments: args } = params;
    try {
      const result = handleToolCall(name, args || {});
      respond(id, {
        content: [{ type: 'text', text: JSON.stringify(result, null, 2) }]
      });
    } catch (e) {
      respond(id, {
        content: [{ type: 'text', text: JSON.stringify({ error: e.message }) }],
        isError: true
      });
    }
    return;
  }

  if (id !== undefined) {
    respondError(id, -32601, `Method not found: ${method}`);
  }
});

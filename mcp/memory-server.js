#!/usr/bin/env node
const readline = require('readline');
const fs = require('fs');
const path = require('path');
const os = require('os');

const MEMORY_DIR = path.join(os.homedir(), '.pulse', 'memory');
const CATEGORIES = ['preference', 'context', 'rule', 'fact'];

function ensureDir(dir) {
  fs.mkdirSync(dir, { recursive: true });
}

function respond(id, result) {
  const msg = { jsonrpc: '2.0', id, result };
  process.stdout.write(JSON.stringify(msg) + '\n');
}

function respondError(id, code, message) {
  const msg = { jsonrpc: '2.0', id, error: { code, message } };
  process.stdout.write(JSON.stringify(msg) + '\n');
}

function memorySave(key, value, category) {
  if (!CATEGORIES.includes(category)) {
    return { error: `Invalid category. Must be one of: ${CATEGORIES.join(', ')}` };
  }
  const dir = path.join(MEMORY_DIR, category);
  ensureDir(dir);
  const filePath = path.join(dir, `${key}.json`);
  const entry = {
    key,
    value,
    category,
    created_at: new Date().toISOString(),
    updated_at: new Date().toISOString()
  };
  if (fs.existsSync(filePath)) {
    try {
      const existing = JSON.parse(fs.readFileSync(filePath, 'utf8'));
      entry.created_at = existing.created_at || entry.created_at;
    } catch (_) {}
  }
  fs.writeFileSync(filePath, JSON.stringify(entry, null, 2));
  return { saved: true, key, category, path: filePath };
}

function memoryRecall(query) {
  const results = [];
  const q = query.toLowerCase();
  for (const cat of CATEGORIES) {
    const dir = path.join(MEMORY_DIR, cat);
    if (!fs.existsSync(dir)) continue;
    const files = fs.readdirSync(dir).filter(f => f.endsWith('.json'));
    for (const file of files) {
      try {
        const data = JSON.parse(fs.readFileSync(path.join(dir, file), 'utf8'));
        const keyMatch = data.key && data.key.toLowerCase().includes(q);
        const valMatch = typeof data.value === 'string' && data.value.toLowerCase().includes(q);
        if (keyMatch || valMatch) {
          results.push(data);
        }
      } catch (_) {}
    }
  }
  return { query, count: results.length, results };
}

function memoryList(category) {
  const results = [];
  const cats = category ? [category] : CATEGORIES;
  for (const cat of cats) {
    const dir = path.join(MEMORY_DIR, cat);
    if (!fs.existsSync(dir)) continue;
    const files = fs.readdirSync(dir).filter(f => f.endsWith('.json'));
    for (const file of files) {
      try {
        const data = JSON.parse(fs.readFileSync(path.join(dir, file), 'utf8'));
        results.push(data);
      } catch (_) {}
    }
  }
  return { count: results.length, memories: results };
}

function memoryForget(key) {
  let deleted = false;
  for (const cat of CATEGORIES) {
    const filePath = path.join(MEMORY_DIR, cat, `${key}.json`);
    if (fs.existsSync(filePath)) {
      fs.unlinkSync(filePath);
      deleted = true;
    }
  }
  return { deleted, key };
}

const TOOLS = [
  {
    name: 'memory_save',
    description: 'Save a fact, preference, context, or rule to persistent memory',
    inputSchema: {
      type: 'object',
      properties: {
        key: { type: 'string', description: 'Unique identifier for this memory' },
        value: { type: 'string', description: 'The content to remember' },
        category: { type: 'string', enum: CATEGORIES, description: 'Category of memory' }
      },
      required: ['key', 'value', 'category']
    }
  },
  {
    name: 'memory_recall',
    description: 'Search memories by keyword (substring match on key and value)',
    inputSchema: {
      type: 'object',
      properties: {
        query: { type: 'string', description: 'Search query' }
      },
      required: ['query']
    }
  },
  {
    name: 'memory_list',
    description: 'List all memories, optionally filtered by category',
    inputSchema: {
      type: 'object',
      properties: {
        category: { type: 'string', enum: CATEGORIES, description: 'Optional category filter' }
      }
    }
  },
  {
    name: 'memory_forget',
    description: 'Delete a specific memory by key',
    inputSchema: {
      type: 'object',
      properties: {
        key: { type: 'string', description: 'Key of the memory to delete' }
      },
      required: ['key']
    }
  }
];

function handleToolCall(name, args) {
  switch (name) {
    case 'memory_save':
      return memorySave(args.key, args.value, args.category);
    case 'memory_recall':
      return memoryRecall(args.query);
    case 'memory_list':
      return memoryList(args.category);
    case 'memory_forget':
      return memoryForget(args.key);
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
      serverInfo: { name: 'pulse-memory', version: '1.0.0' }
    });
    return;
  }

  if (method === 'notifications/initialized') {
    // No response needed for notifications
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

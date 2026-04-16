#!/usr/bin/env node
const readline = require('readline');
const fs = require('fs');
const path = require('path');
const os = require('os');
const https = require('https');

const ACTIONS_LOG = path.join(os.homedir(), '.pulse', 'actions.log');

function respond(id, result) {
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id, result }) + '\n');
}

function respondError(id, code, message) {
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id, error: { code, message } }) + '\n');
}

// Levenshtein distance for typosquatting detection
function levenshtein(a, b) {
  const m = a.length, n = b.length;
  const dp = Array.from({ length: m + 1 }, () => Array(n + 1).fill(0));
  for (let i = 0; i <= m; i++) dp[i][0] = i;
  for (let j = 0; j <= n; j++) dp[0][j] = j;
  for (let i = 1; i <= m; i++) {
    for (let j = 1; j <= n; j++) {
      dp[i][j] = a[i - 1] === b[j - 1]
        ? dp[i - 1][j - 1]
        : 1 + Math.min(dp[i - 1][j], dp[i][j - 1], dp[i - 1][j - 1]);
    }
  }
  return dp[m][n];
}

// Popular packages for typosquatting checks
const POPULAR_PACKAGES = {
  npm: ['express', 'react', 'lodash', 'axios', 'webpack', 'babel', 'typescript', 'eslint', 'prettier', 'jest', 'mocha', 'chalk', 'commander', 'inquirer', 'moment', 'dayjs', 'underscore', 'async', 'debug', 'dotenv', 'cors', 'uuid', 'jsonwebtoken', 'bcrypt', 'mongoose', 'sequelize', 'socket.io', 'next', 'nuxt', 'vue', 'angular', 'svelte'],
  pip: ['requests', 'flask', 'django', 'numpy', 'pandas', 'scipy', 'tensorflow', 'torch', 'boto3', 'pytest', 'setuptools', 'pillow', 'cryptography', 'pyyaml', 'sqlalchemy'],
  cargo: ['serde', 'tokio', 'rand', 'clap', 'reqwest', 'hyper', 'actix-web', 'diesel', 'rocket']
};

function fetchJSON(url) {
  return new Promise((resolve, reject) => {
    https.get(url, { headers: { 'User-Agent': 'pulse-security-mcp/1.0' } }, (res) => {
      let data = '';
      res.on('data', (chunk) => { data += chunk; });
      res.on('end', () => {
        try { resolve(JSON.parse(data)); }
        catch (e) { reject(new Error('Invalid JSON response')); }
      });
    }).on('error', reject);
  });
}

async function checkPackage(name, registry) {
  const reg = registry || 'npm';
  const reasons = [];
  let riskLevel = 'low';
  let safe = true;

  // Check for typosquatting against popular packages
  const popularList = POPULAR_PACKAGES[reg] || [];
  for (const popular of popularList) {
    if (name === popular) continue;
    const dist = levenshtein(name, popular);
    if (dist > 0 && dist <= 2) {
      reasons.push(`Name "${name}" is similar to popular package "${popular}" (edit distance: ${dist}) - possible typosquatting`);
      riskLevel = 'high';
      safe = false;
    }
  }

  // For npm, try to fetch registry data
  if (reg === 'npm') {
    try {
      const data = await fetchJSON(`https://registry.npmjs.org/${encodeURIComponent(name)}`);

      if (data.error) {
        reasons.push(`Package not found on npm registry`);
        riskLevel = 'high';
        safe = false;
      } else {
        // Check age
        const created = new Date(data.time && data.time.created);
        const now = new Date();
        const ageDays = (now - created) / (1000 * 60 * 60 * 24);
        if (ageDays < 7) {
          reasons.push(`Package is only ${Math.floor(ageDays)} days old`);
          riskLevel = riskLevel === 'high' ? 'high' : 'medium';
          safe = false;
        }

        // Check last publish
        const modified = new Date(data.time && data.time.modified);
        const lastPublishDays = (now - modified) / (1000 * 60 * 60 * 24);

        // Check maintainers
        const maintainers = data.maintainers || [];
        if (maintainers.length <= 1) {
          reasons.push(`Only ${maintainers.length} maintainer(s)`);
          if (riskLevel === 'low') riskLevel = 'low'; // single maintainer alone is not risky
        }

        // Check download count via npm API
        try {
          const downloads = await fetchJSON(`https://api.npmjs.org/downloads/point/last-week/${encodeURIComponent(name)}`);
          if (downloads.downloads !== undefined && downloads.downloads < 100) {
            reasons.push(`Very low download count: ${downloads.downloads} downloads last week`);
            riskLevel = riskLevel === 'high' ? 'high' : 'medium';
            safe = false;
          }
        } catch (_) {
          reasons.push('Could not fetch download stats');
        }

        if (reasons.length === 0 || (reasons.length === 1 && reasons[0].includes('maintainer'))) {
          reasons.push(`Package looks legitimate (age: ${Math.floor(ageDays)} days, last updated: ${Math.floor(lastPublishDays)} days ago, maintainers: ${maintainers.length})`);
        }
      }
    } catch (e) {
      reasons.push(`Could not reach npm registry: ${e.message}`);
      riskLevel = 'medium';
    }
  } else {
    // For pip/cargo we only do the typosquatting check
    if (reasons.length === 0) {
      reasons.push(`No typosquatting detected for ${reg} package "${name}". Note: registry API checks only available for npm.`);
    }
  }

  return { safe, risk_level: riskLevel, reasons };
}

// Dangerous command patterns
const DANGEROUS_PATTERNS = [
  { pattern: /rm\s+(-[a-zA-Z]*f[a-zA-Z]*\s+)?(-[a-zA-Z]*r[a-zA-Z]*\s+)?\/\s*$|rm\s+-rf\s+\//, reason: 'Recursive delete of root filesystem' },
  { pattern: /rm\s+-rf\s+(~|\/home|\$HOME)/, reason: 'Recursive delete of home directory' },
  { pattern: /mkfs\./, reason: 'Formatting a filesystem' },
  { pattern: /dd\s+.*of=\/dev\/[sh]d/, reason: 'Direct disk write that can destroy data' },
  { pattern: /:\s*\(\)\s*\{\s*:\s*\|\s*:\s*&\s*\}\s*;\s*:/, reason: 'Fork bomb' },
  { pattern: />\s*\/dev\/[sh]d/, reason: 'Overwriting disk device' },
  { pattern: /chmod\s+(-R\s+)?777/, reason: 'Setting overly permissive file permissions (777)' },
  { pattern: /chmod\s+(-R\s+)?a\+rwx/, reason: 'Setting overly permissive file permissions' },
  { pattern: /curl\s+.*\|\s*(ba)?sh/, reason: 'Piping remote content directly to shell execution' },
  { pattern: /wget\s+.*\|\s*(ba)?sh/, reason: 'Piping remote content directly to shell execution' },
  { pattern: /curl\s+.*\|\s*sudo\s+(ba)?sh/, reason: 'Piping remote content to privileged shell' },
  { pattern: /eval\s*\(\s*\$\(curl/, reason: 'Evaluating remote content' },
  { pattern: /python\s+-c\s+.*import\s+os/, reason: 'Inline Python with OS access' },
  { pattern: />\s*\/etc\/(passwd|shadow|sudoers)/, reason: 'Overwriting critical system file' },
  { pattern: /echo\s+.*>\s*\/etc\//, reason: 'Writing to system configuration' }
];

// Environment variable exfiltration patterns
const ENV_EXFIL_PATTERNS = [
  /\$(API_KEY|SECRET|TOKEN|PASSWORD|PRIVATE_KEY|AWS_SECRET|AWS_ACCESS|GITHUB_TOKEN|NPM_TOKEN|DB_PASSWORD|DATABASE_URL|CREDENTIALS)/i,
  /\$\{(API_KEY|SECRET|TOKEN|PASSWORD|PRIVATE_KEY|AWS_SECRET|AWS_ACCESS|GITHUB_TOKEN|NPM_TOKEN|DB_PASSWORD|DATABASE_URL|CREDENTIALS)\}/i
];

function checkCommand(command) {
  const reasons = [];
  let riskLevel = 'low';
  let safe = true;

  for (const { pattern, reason } of DANGEROUS_PATTERNS) {
    if (pattern.test(command)) {
      reasons.push(reason);
      riskLevel = 'high';
      safe = false;
    }
  }

  // Check for env var exfiltration in curl/wget commands
  if (/curl|wget|fetch|http/.test(command)) {
    for (const pattern of ENV_EXFIL_PATTERNS) {
      if (pattern.test(command)) {
        reasons.push('Potential secret exfiltration: sensitive environment variable used in network command');
        riskLevel = 'high';
        safe = false;
        break;
      }
    }
  }

  // Check for sudo usage
  if (/sudo\s/.test(command) && reasons.length === 0) {
    reasons.push('Command uses sudo (elevated privileges)');
    if (riskLevel === 'low') riskLevel = 'medium';
  }

  if (reasons.length === 0) {
    reasons.push('No dangerous patterns detected');
  }

  return { safe, risk_level: riskLevel, reasons };
}

// Secret detection patterns
const SECRET_PATTERNS = [
  { name: 'AWS Access Key', pattern: /AKIA[0-9A-Z]{16}/, severity: 'high' },
  { name: 'AWS Secret Key', pattern: /aws_secret_access_key\s*=\s*[A-Za-z0-9/+=]{40}/i, severity: 'high' },
  { name: 'GitHub Token', pattern: /ghp_[A-Za-z0-9]{36}/, severity: 'high' },
  { name: 'GitHub OAuth', pattern: /gho_[A-Za-z0-9]{36}/, severity: 'high' },
  { name: 'GitHub App Token', pattern: /ghu_[A-Za-z0-9]{36}/, severity: 'high' },
  { name: 'GitHub Fine-grained Token', pattern: /github_pat_[A-Za-z0-9_]{22,}/, severity: 'high' },
  { name: 'Slack Token', pattern: /xox[bpors]-[A-Za-z0-9-]+/, severity: 'high' },
  { name: 'Slack Webhook', pattern: /hooks\.slack\.com\/services\/T[A-Z0-9]+\/B[A-Z0-9]+\/[A-Za-z0-9]+/, severity: 'high' },
  { name: 'Private Key', pattern: /-----BEGIN (RSA |EC |DSA |OPENSSH )?PRIVATE KEY-----/, severity: 'critical' },
  { name: 'Generic API Key', pattern: /api[_-]?key\s*[:=]\s*['"][A-Za-z0-9]{16,}['"]/i, severity: 'medium' },
  { name: 'Generic Secret', pattern: /secret\s*[:=]\s*['"][A-Za-z0-9]{16,}['"]/i, severity: 'medium' },
  { name: 'Generic Password', pattern: /password\s*[:=]\s*['"][^'"]{8,}['"]/i, severity: 'medium' },
  { name: 'Generic Token', pattern: /token\s*[:=]\s*['"][A-Za-z0-9]{16,}['"]/i, severity: 'medium' },
  { name: 'Connection String', pattern: /(mongodb|postgres|mysql|redis):\/\/[^:]+:[^@]+@/i, severity: 'high' },
  { name: 'Bearer Token', pattern: /Bearer\s+[A-Za-z0-9\-._~+/]+=*/i, severity: 'medium' }
];

function scanFile(filePath) {
  const resolved = path.resolve(filePath);
  if (!fs.existsSync(resolved)) {
    return { clean: true, findings: [], error: 'File not found' };
  }

  let content;
  try {
    content = fs.readFileSync(resolved, 'utf8');
  } catch (e) {
    return { clean: true, findings: [], error: `Cannot read file: ${e.message}` };
  }

  const lines = content.split('\n');
  const findings = [];

  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    for (const { name, pattern, severity } of SECRET_PATTERNS) {
      if (pattern.test(line)) {
        findings.push({
          line: i + 1,
          pattern: name,
          severity,
          preview: line.substring(0, 80) + (line.length > 80 ? '...' : '')
        });
      }
    }
  }

  return { clean: findings.length === 0, findings, file: resolved, lines_scanned: lines.length };
}

function auditRecent() {
  if (!fs.existsSync(ACTIONS_LOG)) {
    return { audited: 0, findings: [], note: 'No actions.log found at ' + ACTIONS_LOG };
  }

  let content;
  try {
    content = fs.readFileSync(ACTIONS_LOG, 'utf8');
  } catch (e) {
    return { audited: 0, findings: [], error: `Cannot read actions.log: ${e.message}` };
  }

  const lines = content.trim().split('\n').filter(Boolean);
  const findings = [];

  for (const line of lines.slice(-50)) { // Check last 50 actions
    let action;
    try {
      action = JSON.parse(line);
    } catch (_) {
      // Try treating line as a plain command
      action = { command: line, timestamp: 'unknown' };
    }

    if (action.command) {
      const check = checkCommand(action.command);
      if (!check.safe) {
        findings.push({
          action: action.command,
          timestamp: action.timestamp || 'unknown',
          risk_level: check.risk_level,
          reasons: check.reasons
        });
      }
    }
  }

  return {
    audited: lines.length,
    checked: Math.min(lines.length, 50),
    risky_actions: findings.length,
    findings
  };
}

const TOOLS = [
  {
    name: 'security_check_package',
    description: 'Check if a package is safe to install. Checks for typosquatting, download count, age, and maintainer info.',
    inputSchema: {
      type: 'object',
      properties: {
        name: { type: 'string', description: 'Package name to check' },
        registry: { type: 'string', enum: ['npm', 'pip', 'cargo'], description: 'Package registry (default: npm)' }
      },
      required: ['name']
    }
  },
  {
    name: 'security_check_command',
    description: 'Check if a shell command is dangerous. Detects destructive commands, secret exfiltration, and unsafe patterns.',
    inputSchema: {
      type: 'object',
      properties: {
        command: { type: 'string', description: 'Shell command to check' }
      },
      required: ['command']
    }
  },
  {
    name: 'security_scan_file',
    description: 'Scan a file for hardcoded secrets, API keys, tokens, and passwords.',
    inputSchema: {
      type: 'object',
      properties: {
        path: { type: 'string', description: 'Path to the file to scan' }
      },
      required: ['path']
    }
  },
  {
    name: 'security_audit_recent',
    description: 'Audit recent logged actions for safety issues. Reads ~/.pulse/actions.log.',
    inputSchema: { type: 'object', properties: {} }
  }
];

async function handleToolCall(name, args) {
  switch (name) {
    case 'security_check_package':
      return await checkPackage(args.name, args.registry);
    case 'security_check_command':
      return checkCommand(args.command);
    case 'security_scan_file':
      return scanFile(args.path);
    case 'security_audit_recent':
      return auditRecent();
    default:
      return { error: `Unknown tool: ${name}` };
  }
}

const rl = readline.createInterface({ input: process.stdin });

rl.on('line', async (line) => {
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
      serverInfo: { name: 'pulse-security', version: '1.0.0' }
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
      const result = await handleToolCall(name, args || {});
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

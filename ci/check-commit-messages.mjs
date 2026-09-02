#!/usr/bin/env node

import { readFileSync } from "node:fs";
import { basename } from "node:path";
import { spawnSync } from "node:child_process";

const subjectPattern = /^(build|chore|ci|deprecate|docs|feat|fix|perf|refactor|revert|security|style|test)(\([A-Za-z0-9._/-]+\))?(!)?:\s+.+/u;
const breakingPattern = /^(build|chore|ci|deprecate|docs|feat|fix|perf|refactor|revert|security|style|test)(\([A-Za-z0-9._/-]+\))?!:\s+/u;
const trailerPattern = /^[A-Za-z0-9-]+:\s+.+/u;

let status = 0;
let totalBodyChars = 0;

function fail(message) {
  process.stderr.write(`${message}\n`);
  status = 1;
}

function git(args, options = {}) {
  const result = spawnSync("git", args, {
    encoding: "utf8",
    input: options.input,
  });

  if (result.error) {
    throw result.error;
  }
  if (result.status !== 0) {
    const detail = result.stderr.trim();
    throw new Error(detail || `git ${args.join(" ")} failed`);
  }
  return result.stdout;
}

function stripTrailerBlock(body) {
  const lines = body.replaceAll("\r\n", "\n").split("\n");
  while (lines.at(-1) === "") {
    lines.pop();
  }

  const lastBlank = lines.lastIndexOf("");
  const trailer = lines.slice(lastBlank + 1);
  let haveTrailer = false;
  let valid = trailer.length > 0;

  for (const line of trailer) {
    if (trailerPattern.test(line)) {
      haveTrailer = true;
    } else if (haveTrailer && /^\s+/u.test(line)) {
      continue;
    } else {
      valid = false;
      break;
    }
  }

  if (valid && haveTrailer) {
    lines.splice(Math.max(0, lastBlank));
  }
  while (lines.at(-1) === "") {
    lines.pop();
  }
  return lines.join("\n");
}

function checkMessage(source, message) {
  const normalized = message.replaceAll("\r\n", "\n").replace(/\n+$/u, "");
  const newline = normalized.indexOf("\n");
  const subject = newline === -1 ? normalized : normalized.slice(0, newline);
  let body = newline === -1 ? "" : normalized.slice(newline + 1);
  if (body.startsWith("\n")) {
    body = body.slice(1);
  }
  const explanatoryBody = stripTrailerBlock(body);

  if (!subjectPattern.test(subject)) {
    fail(`${source}: subject is not a Conventional Commit: ${subject}`);
  }
  if (breakingPattern.test(subject) && !/\S/u.test(explanatoryBody)) {
    fail(`${source}: breaking commit requires an explanatory body`);
  }

  const bodyChars = [...explanatoryBody].length;
  totalBodyChars += bodyChars;
  if (bodyChars > 500) {
    fail(`${source}: body has ${bodyChars} characters; maximum is 500`);
  }
}

function usage() {
  process.stderr.write(
    `usage: ${process.argv[1]} <base> <head>\n` +
      `       ${process.argv[1]} --message-file <path>\n`,
  );
  process.exit(2);
}

const args = process.argv.slice(2);

try {
  if (args.length === 2 && args[0] === "--message-file") {
    let contents;
    try {
      contents = readFileSync(args[1], "utf8");
    } catch {
      process.stderr.write(
        `commit message check: message file does not exist: ${args[1]}\n`,
      );
      process.exit(2);
    }

    const message = git(["stripspace", "--strip-comments"], { input: contents });
    checkMessage(basename(args[1]), message);
    process.exit(status);
  }

  if (args.length !== 2) {
    usage();
  }

  const [base, head] = args;
  for (const revision of [base, head]) {
    const result = spawnSync("git", ["rev-parse", "--verify", `${revision}^{commit}`], {
      encoding: "utf8",
      stdio: "ignore",
    });
    if (result.status !== 0) {
      process.stderr.write(`commit message check: invalid revision: ${revision}\n`);
      process.exit(2);
    }
  }

  const commits = git(["rev-list", "--reverse", "--no-merges", `${base}..${head}`])
    .trim()
    .split("\n")
    .filter(Boolean);

  for (const commit of commits) {
    const short = git(["rev-parse", "--short=12", commit]).trim();
    const message = git(["show", "-s", "--format=%B", commit]);
    checkMessage(short, message);
  }

  if (totalBodyChars > 1000) {
    fail(
      `commit body total is ${totalBodyChars} characters; ` +
        "maximum per pull request is 1000",
    );
  }
} catch (error) {
  process.stderr.write(`commit message check: ${error.message}\n`);
  process.exit(2);
}

process.exit(status);

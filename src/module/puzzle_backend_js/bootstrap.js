(() => {
  'use strict';

  const nativeCall = globalThis.__rbph_native_call;
  const metadata = globalThis.__rbph_bootstrap_metadata;
  delete globalThis.__rbph_native_call;
  delete globalThis.__rbph_bootstrap_metadata;

  const call = (operation, payload) => nativeCall(operation, payload);
  const fail = message => {
    throw new TypeError(message);
  };
  const isObject = value => value !== null && typeof value === 'object' && !Array.isArray(value);
  const requireString = (value, message) => {
    if (typeof value !== 'string') fail(message);
    return value;
  };
  const requireJson = value => {
    if (value === undefined) fail('value is undefined');
    return value;
  };
  const positiveI32 = (value, message) => {
    if (typeof value !== 'number' || !Number.isFinite(value) || !Number.isInteger(value) || value < 1 || value > 2147483647) {
      fail(message);
    }
    return value;
  };
  const integerI64 = (value, message) => {
    if (typeof value !== 'number' || !Number.isFinite(value) || !Number.isInteger(value) || value < -9223372036854775808 || value > 9223372036854775808) {
      fail(message);
    }
    return rustI64String(value);
  };
  const rustI64String = value => {
    if (Number.isNaN(value)) return '0';
    if (value >= 9223372036854775808) return '9223372036854775807';
    if (value <= -9223372036854775808) return '-9223372036854775808';
    return BigInt(Math.trunc(value)).toString();
  };

  const scopeFromValue = value => {
    if (!isObject(value)) fail('scope must be an object');
    switch (value.type) {
      case 'global':
        return { type: 'global' };
      case 'team':
        if (!Object.prototype.hasOwnProperty.call(value, 'teamId') || typeof value.teamId !== 'number' || !Number.isInteger(value.teamId)) {
          fail('team scope requires teamId');
        }
        return { type: 'team', teamId: positiveI32(value.teamId, 'teamId must be a positive integer in i32 range') };
      case 'puzzle':
        if (!Object.prototype.hasOwnProperty.call(value, 'puzzleId') || typeof value.puzzleId !== 'number' || !Number.isInteger(value.puzzleId)) {
          fail('puzzle scope requires puzzleId');
        }
        return { type: 'puzzle', puzzleId: positiveI32(value.puzzleId, 'puzzleId must be a positive integer in i32 range') };
      case 'teamPuzzle':
        if (!Object.prototype.hasOwnProperty.call(value, 'teamId') || typeof value.teamId !== 'number' || !Number.isInteger(value.teamId)) {
          fail('teamPuzzle scope requires teamId');
        }
        if (!Object.prototype.hasOwnProperty.call(value, 'puzzleId') || typeof value.puzzleId !== 'number' || !Number.isInteger(value.puzzleId)) {
          fail('teamPuzzle scope requires puzzleId');
        }
        return {
          type: 'teamPuzzle',
          teamId: positiveI32(value.teamId, 'teamId must be a positive integer in i32 range'),
          puzzleId: positiveI32(value.puzzleId, 'puzzleId must be a positive integer in i32 range'),
        };
      default:
        fail('scope.type must be global, team, puzzle, or teamPuzzle');
    }
  };

  const implicitScope = {
    team: { type: 'team', teamId: metadata.team.id },
    puzzle: { type: 'puzzle', puzzleId: metadata.puzzle.id },
    this: { type: 'teamPuzzle', teamId: metadata.team.id, puzzleId: metadata.puzzle.id },
  };
  const scopeProvider = (selector, args) => {
    if (selector === 'game') {
      if (args.length === 0) fail('$game scope argument is required');
      return { scope: scopeFromValue(args[0]), offset: 1 };
    }
    return { scope: implicitScope[selector], offset: 0 };
  };

  const MAX_KV_TTL_MS = 365 * 24 * 60 * 60 * 1000;
  const kvExpiry = (value, omittedMode, label) => {
    if (value === undefined || value === null) return { mode: omittedMode };
    if (!isObject(value)) fail(`${label} options must be an object`);
    if (!Object.prototype.hasOwnProperty.call(value, 'ttl')) return { mode: omittedMode };
    if (value.ttl === null) return { mode: 'permanent' };
    if (typeof value.ttl !== 'number' || !Number.isInteger(value.ttl) || value.ttl < 1 || value.ttl > MAX_KV_TTL_MS) {
      fail(`${label} options.ttl must be an integer between 1 and ${MAX_KV_TTL_MS} milliseconds`);
    }
    return { mode: 'ttl', ttlMs: value.ttl };
  };

  const makeKv = (selector, label) => ({
    get: function get(scopeOrKey, key) {
      const selected = scopeProvider(selector, arguments);
      const actualKey = requireString(arguments[selected.offset], `${label}.kv.get requires a key`);
      return call('kvGet', { scope: selected.scope, key: actualKey });
    },
    getEntry: function getEntry(scopeOrKey, key) {
      const selected = scopeProvider(selector, arguments);
      const actualKey = requireString(arguments[selected.offset], `${label}.kv.getEntry requires a key`);
      return call('kvGetEntry', { scope: selected.scope, key: actualKey });
    },
    set: function set(scopeOrKey, keyOrValue, valueOrOptions, options) {
      const selected = scopeProvider(selector, arguments);
      const actualKey = requireString(arguments[selected.offset], `${label}.kv.set requires a key`);
      const value = selected.offset + 1 < arguments.length ? requireJson(arguments[selected.offset + 1]) : null;
      return call('kvSet', {
        scope: selected.scope,
        key: actualKey,
        value,
        expiry: kvExpiry(arguments[selected.offset + 2], 'preserve', `${label}.kv.set`),
      });
    },
    increment: function increment(scopeOrKey, keyOrAmount, amountOrOptions, options) {
      const selected = scopeProvider(selector, arguments);
      const actualKey = requireString(arguments[selected.offset], `${label}.kv.increment requires a key`);
      const rawAmount = arguments[selected.offset + 1];
      const amount = rawAmount === undefined ? 1 : rawAmount;
      if (typeof amount !== 'number' || !Number.isFinite(amount)) {
        fail(`${label}.kv.increment amount must be a finite number`);
      }
      return call('kvIncrement', {
        scope: selected.scope,
        key: actualKey,
        amount,
        expiry: kvExpiry(arguments[selected.offset + 2], 'preserve', `${label}.kv.increment`),
      });
    },
    setIfAbsent: function setIfAbsent(scopeOrKey, keyOrValue, valueOrOptions, options) {
      const selected = scopeProvider(selector, arguments);
      const actualKey = requireString(arguments[selected.offset], `${label}.kv.setIfAbsent requires a key`);
      const value = selected.offset + 1 < arguments.length ? requireJson(arguments[selected.offset + 1]) : null;
      return call('kvSetIfAbsent', {
        scope: selected.scope,
        key: actualKey,
        value,
        expiry: kvExpiry(arguments[selected.offset + 2], 'permanent', `${label}.kv.setIfAbsent`),
      });
    },
    compareAndSet: function compareAndSet(scopeOrKey, keyOrVersion, versionOrValue, valueOrOptions, options) {
      const selected = scopeProvider(selector, arguments);
      const actualKey = requireString(arguments[selected.offset], `${label}.kv.compareAndSet requires a key`);
      const version = requireString(arguments[selected.offset + 1], `${label}.kv.compareAndSet requires an expected version string`);
      if (!/^[1-9][0-9]*$/.test(version)) {
        fail(`${label}.kv.compareAndSet expected version is invalid`);
      }
      try {
        if (BigInt(version) > 9223372036854775807n) fail(`${label}.kv.compareAndSet expected version is invalid`);
      } catch (_) {
        fail(`${label}.kv.compareAndSet expected version is invalid`);
      }
      const value = selected.offset + 2 < arguments.length ? requireJson(arguments[selected.offset + 2]) : null;
      return call('kvCompareAndSet', {
        scope: selected.scope,
        key: actualKey,
        expectedVersion: version,
        value,
        expiry: kvExpiry(arguments[selected.offset + 3], 'preserve', `${label}.kv.compareAndSet`),
      });
    },
    delete: function deleteValue(scopeOrKey, key) {
      const selected = scopeProvider(selector, arguments);
      const actualKey = requireString(arguments[selected.offset], `${label}.kv.delete requires a key`);
      return call('kvDelete', { scope: selected.scope, key: actualKey });
    },
  });

  const validStoreName = (name, message) => {
    if (name.length < 1 || name.length > 64 || !/^[A-Za-z0-9_.-]+$/.test(name)) fail(message);
  };
  const storeSchema = value => {
    if (value === undefined || value === null || !isObject(value) || !isObject(value.indexes)) {
      return { indexes: {} };
    }
    return { indexes: value.indexes };
  };
  const storeOptions = value => {
    if (value === undefined) return { where: {} };
    if (!isObject(value)) fail('$store.collection(...).list options must be an object');
    let where = {};
    if (Object.prototype.hasOwnProperty.call(value, 'where')) {
      if (!isObject(value.where)) fail('$store list where must be an object');
      where = value.where;
    }
    return {
      where,
      limit: typeof value.limit === 'number' && Number.isInteger(value.limit) && value.limit >= -9223372036854775808 && value.limit < 9223372036854775808 ? value.limit : null,
      cursor: Object.prototype.hasOwnProperty.call(value, 'cursor') && value.cursor !== undefined ? value.cursor : null,
      order: typeof value.order === 'string' ? value.order : null,
    };
  };
  const makeStore = (selector, label) => ({
    collection: function collection(scopeOrName, nameOrSchema, schemaArg) {
      const selected = scopeProvider(selector, arguments);
      const name = requireString(arguments[selected.offset], `${label}.store.collection requires a collection name`);
      validStoreName(name, '$store collection name must be 1-64 chars using letters, numbers, _, -, or .');
      if (arguments.length > selected.offset + 1 && arguments[selected.offset + 1] === undefined) fail('value is undefined');
      const schema = storeSchema(arguments[selected.offset + 1]);
      return {
        insert: function insert(value) {
          const actual = arguments.length === 0 ? null : requireJson(value);
          if (!isObject(actual)) fail('$store.collection(...).insert requires an object value');
          return call('storeInsert', { scope: selected.scope, collection: name, schema, value: actual });
        },
        get: function get(docId) {
          if (typeof docId !== 'number') fail('$store.collection(...).get requires a document id');
          return call('storeGet', {
            scope: selected.scope,
            collection: name,
            docId: rustI64String(docId),
          });
        },
        list: function list(options) {
          if (arguments.length > 0 && options === undefined) fail('value is undefined');
          return call('storeList', {
            scope: selected.scope,
            collection: name,
            schema,
            options: storeOptions(options),
          });
        },
      };
    },
  });

  const currencyRef = (value, message) => {
    if (typeof value === 'number') {
      return { kind: 'id', value: positiveI32(value, 'currency id must be a positive integer in i32 range') };
    }
    if (typeof value === 'string') return { kind: 'slug', value };
    fail(message);
  };
  const currencyTeam = (selector, args) => {
    if (selector === 'game') {
      if (typeof args[0] !== 'number') fail('$game.currency requires team id');
      return {
        teamId: positiveI32(args[0], '$game.currency team id must be a positive integer in i32 range'),
        offset: 1,
        checkTeam: true,
      };
    }
    return { teamId: metadata.team.id, offset: 0, checkTeam: false };
  };
  const optionalReason = (value, message) => {
    if (value === undefined || value === null) return null;
    if (typeof value !== 'string') fail(message);
    return value;
  };
  const currencyUpdate = value => {
    if (typeof value === 'number') {
      return { amount: integerI64(value, 'currency.update amount must be an integer in i64 range') };
    }
    if (!isObject(value)) fail('currency.update options must be an object');
    const result = {};
    for (const [publicName, wireName] of [
      ['amount', 'amount'],
      ['teamGrowth', 'teamGrowth'],
    ]) {
      if (Object.prototype.hasOwnProperty.call(value, publicName)) {
        const field = value[publicName];
        if (typeof field !== 'number' || !Number.isFinite(field) || !Number.isInteger(field) || field < -9223372036854775808 || field > 9223372036854775808) {
          fail(`currency.update options.${publicName} must be a number`);
        }
        if (field >= 9223372036854775808) fail(`currency.update options.${publicName} must be a number`);
        result[wireName] = rustI64String(field);
      }
    }
    if (Object.prototype.hasOwnProperty.call(value, 'hidden')) {
      if (typeof value.hidden !== 'boolean') fail('currency.update options.hidden must be a boolean');
      result.hidden = value.hidden;
    }
    return result;
  };
  const makeCurrency = selector => ({
    query: function query(teamOrCurrency, currencyArg) {
      const selected = currencyTeam(selector, arguments);
      const hasCurrency = arguments.length > selected.offset;
      return call('currencyQuery', {
        teamId: selected.teamId,
        checkTeam: selected.checkTeam,
        currency: hasCurrency ? currencyRef(arguments[selected.offset], 'currency.query requires currency id or slug') : null,
      });
    },
    cost: function cost(teamOrCurrency, currencyOrAmount, amountOrReason, reasonArg) {
      const selected = currencyTeam(selector, arguments);
      const currency = currencyRef(arguments[selected.offset], 'currency.cost requires currency id or slug');
      if (typeof arguments[selected.offset + 1] !== 'number') fail('currency.cost requires amount');
      const amount = integerI64(arguments[selected.offset + 1], 'currency.cost amount must be an integer in i64 range');
      return call('currencyCost', {
        teamId: selected.teamId,
        checkTeam: selected.checkTeam,
        currency,
        amount,
        reason: optionalReason(arguments[selected.offset + 2], 'currency.cost reason must be a string or null'),
      });
    },
    add: function add(teamOrCurrency, currencyOrAmount, amountOrReason, reasonArg) {
      const selected = currencyTeam(selector, arguments);
      const currency = currencyRef(arguments[selected.offset], 'currency.add requires currency id or slug');
      if (typeof arguments[selected.offset + 1] !== 'number') fail('currency.add requires amount');
      const amount = integerI64(arguments[selected.offset + 1], 'currency.add amount must be an integer in i64 range');
      return call('currencyAdd', {
        teamId: selected.teamId,
        checkTeam: selected.checkTeam,
        currency,
        amount,
        reason: optionalReason(arguments[selected.offset + 2], 'currency.add reason must be a string or null'),
      });
    },
    update: function update(teamOrCurrency, currencyOrOptions, optionsOrReason, reasonArg) {
      const selected = currencyTeam(selector, arguments);
      const currency = currencyRef(arguments[selected.offset], 'currency.update requires currency id or slug');
      if (arguments.length <= selected.offset + 1) fail('currency.update requires amount or options');
      return call('currencyUpdate', {
        teamId: selected.teamId,
        checkTeam: selected.checkTeam,
        currency,
        options: currencyUpdate(arguments[selected.offset + 1]),
        reason: optionalReason(arguments[selected.offset + 2], 'currency.update reason must be a string or null'),
      });
    },
  });

  const utf8Length = value => {
    let length = 0;
    for (const character of value) {
      const point = character.codePointAt(0);
      length += point <= 0x7f ? 1 : point <= 0x7ff ? 2 : point <= 0xffff ? 3 : 4;
    }
    return length;
  };
  const assetPath = (value, message) => {
    const path = requireString(value, message);
    if (path.length === 0 || utf8Length(path) > 1024 || path.includes('\0')) fail(message);
    return path;
  };
  const assets = {
    list: function list(objectKey) {
      return call('assetList', {
        objectKey: requireString(objectKey, '$puzzle.assets.list requires an object key'),
      });
    },
    readText: function readText(objectKey, relativePath) {
      return call('assetReadText', {
        objectKey: requireString(objectKey, '$puzzle.assets.readText requires an object key'),
        relativePath: assetPath(relativePath, '$puzzle.assets.readText requires a relative path'),
      });
    },
    readJson: function readJson(objectKey, relativePath) {
      return call('assetReadJson', {
        objectKey: requireString(objectKey, '$puzzle.assets.readJson requires an object key'),
        relativePath: assetPath(relativePath, '$puzzle.assets.readJson requires a relative path'),
      });
    },
    readBytes: function readBytes(objectKey, relativePath) {
      return call('assetReadBytes', {
        objectKey: requireString(objectKey, '$puzzle.assets.readBytes requires an object key'),
        relativePath: assetPath(relativePath, '$puzzle.assets.readBytes requires a relative path'),
      });
    },
  };

  const consoleObject = {};
  for (const level of ['debug', 'log', 'info', 'warn', 'error']) {
    consoleObject[level] = function consoleWrite(value) {
      const parts = [];
      for (const item of arguments) {
        try {
          parts.push(String(item));
        } catch (error) {
          parts.push(`<unprintable: ${error}>`);
        }
      }
      call('consoleWrite', { level, message: parts.join(' ') });
    };
  }

  const game = {
    id: metadata.gameId,
    kv: makeKv('game', '$game'),
    store: makeStore('game', '$game'),
    currency: makeCurrency('game'),
  };
  const team = {
    id: metadata.team.id,
    name: metadata.team.name,
    kv: makeKv('team', '$team'),
    store: makeStore('team', '$team'),
    currency: makeCurrency('team'),
  };
  const puzzle = {
    id: metadata.puzzle.id,
    gameId: metadata.gameId,
    title: metadata.puzzle.title,
    kv: makeKv('puzzle', '$puzzle'),
    store: makeStore('puzzle', '$puzzle'),
    assets,
  };
  const thisObject = {
    game,
    team,
    puzzle,
    kv: makeKv('this', '$this'),
    store: makeStore('this', '$this'),
    submission: {
      add: function add(submission) {
        if (arguments.length === 0) fail('$this.submission.add requires an object');
        return call('submissionAdd', { submission: requireJson(submission) });
      },
    },
    event: {
      emit: function emit(event, payload) {
        const name = requireString(event, '$this.event.emit requires an event name');
        const actualPayload = arguments.length < 2 ? null : requireJson(payload);
        return call('eventEmit', { event: name, payload: actualPayload });
      },
    },
    solve: function solve(submission) {
      if (arguments.length === 0) fail('$this.solve requires a submission');
      return call('puzzleSolve', { submission: requireJson(submission) });
    },
  };

  for (const [name, value] of [
    ['$game', game],
    ['$team', team],
    ['$puzzle', puzzle],
    ['$this', thisObject],
    ['console', consoleObject],
  ]) {
    Object.defineProperty(globalThis, name, {
      value,
      writable: true,
      enumerable: true,
      configurable: true,
    });
  }
})();

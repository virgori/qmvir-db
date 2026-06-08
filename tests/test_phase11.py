"""Phase 11 — Stored Procedures, Triggers & Event-Driven Pipelines.

Tests cover:
    1. PL/QM interpreter: variables, control flow, expressions, built-ins, error handling
    2. Procedure catalog: register, call, drop, parameter validation, stats
    3. Trigger system: BEFORE/AFTER INSERT/UPDATE/DELETE, cancellation, priority, conditional
    4. Event bus: publish/subscribe, filtering, history, unsubscribe
    5. Event pipelines: filter→transform→sink chains, attach/detach, error handling
"""

import time
import pytest


# ─── 1. PL/QM Interpreter ──────────────────────────────────────────

class TestPLQMInterpreter:
    """PL/QM Language interpreter tests."""

    def _exec(self, source, params=None):
        from qm_core.procedures.plqm import PLQMInterpreter
        return PLQMInterpreter().execute(source, params=params)

    # Variables & assignment
    def test_declare_and_return(self):
        src = "DECLARE x INT = 42; RETURN x;"
        assert self._exec(src) == 42

    def test_declare_default_none(self):
        src = "DECLARE x; RETURN x;"
        assert self._exec(src) is None

    def test_set_variable(self):
        src = "DECLARE x = 1; SET x = 99; RETURN x;"
        assert self._exec(src) == 99

    def test_params_injection(self):
        src = "RETURN n + 1;"
        assert self._exec(src, {"n": 10}) == 11

    # Arithmetic
    def test_arithmetic_precedence(self):
        assert self._exec("RETURN 2 + 3 * 4;") == 14

    def test_parentheses(self):
        assert self._exec("RETURN (2 + 3) * 4;") == 20

    def test_division(self):
        assert self._exec("RETURN 10 / 4;") == 2.5

    def test_modulo(self):
        assert self._exec("RETURN 10 % 3;") == 1

    def test_unary_minus(self):
        assert self._exec("RETURN -5 + 3;") == -2

    def test_division_by_zero(self):
        from qm_core.procedures.plqm import PLQMError
        with pytest.raises(PLQMError, match="Division by zero"):
            self._exec("RETURN 1 / 0;")

    # Comparison operators
    def test_eq(self):
        assert self._exec("RETURN 1 = 1;") is True

    def test_neq(self):
        assert self._exec("RETURN 1 != 2;") is True

    def test_lt_gt(self):
        assert self._exec("RETURN 1 < 2;") is True
        assert self._exec("RETURN 2 > 1;") is True

    def test_lte_gte(self):
        assert self._exec("RETURN 2 <= 2;") is True
        assert self._exec("RETURN 3 >= 3;") is True

    # Boolean logic
    def test_and(self):
        assert self._exec("RETURN 1 = 1 AND 2 = 2;") is True
        assert self._exec("RETURN 1 = 1 AND 2 = 3;") is False

    def test_or(self):
        assert self._exec("RETURN 1 = 2 OR 2 = 2;") is True

    def test_not(self):
        assert self._exec("RETURN NOT FALSE;") is True

    def test_is_null(self):
        src = "DECLARE x; RETURN x IS NULL;"
        assert self._exec(src) is True

    def test_is_not_null(self):
        src = "DECLARE x = 1; RETURN x IS NOT NULL;"
        assert self._exec(src) is True

    # String operations
    def test_string_concat(self):
        assert self._exec("RETURN 'hello' + ' ' + 'world';") == "hello world"

    def test_string_number_concat(self):
        assert self._exec("RETURN 'count: ' + 42;") == "count: 42"

    # Control flow: IF
    def test_if_then(self):
        src = """
        DECLARE x = 10;
        IF x > 5 THEN
            RETURN 'big';
        END IF;
        RETURN 'small';
        """
        assert self._exec(src) == "big"

    def test_if_else(self):
        src = """
        DECLARE x = 3;
        IF x > 5 THEN
            RETURN 'big';
        ELSE
            RETURN 'small';
        END IF;
        """
        assert self._exec(src) == "small"

    def test_if_elif(self):
        src = """
        DECLARE x = 5;
        IF x > 10 THEN
            RETURN 'A';
        ELIF x > 3 THEN
            RETURN 'B';
        ELIF x > 1 THEN
            RETURN 'C';
        ELSE
            RETURN 'D';
        END IF;
        """
        assert self._exec(src) == "B"

    # Control flow: WHILE
    def test_while_loop(self):
        src = """
        DECLARE i = 0;
        DECLARE total = 0;
        WHILE i < 5 DO
            SET total = total + i;
            SET i = i + 1;
        END WHILE;
        RETURN total;
        """
        assert self._exec(src) == 10  # 0+1+2+3+4

    def test_while_infinite_guard(self):
        from qm_core.procedures.plqm import PLQMError
        src = "DECLARE x = 1; WHILE x > 0 DO SET x = x + 1; END WHILE;"
        with pytest.raises(PLQMError, match="Maximum iterations"):
            self._exec(src)

    # Control flow: FOR
    def test_for_loop(self):
        src = """
        DECLARE total = 0;
        FOR i IN range(5) DO
            SET total = total + i;
        END FOR;
        RETURN total;
        """
        assert self._exec(src) == 10

    # Built-in functions
    def test_builtin_len(self):
        assert self._exec("RETURN len('hello');") == 5

    def test_builtin_abs(self):
        assert self._exec("RETURN abs(-7);") == 7

    def test_builtin_upper_lower(self):
        assert self._exec("RETURN upper('hello');") == "HELLO"
        assert self._exec("RETURN lower('WORLD');") == "world"

    def test_builtin_coalesce(self):
        assert self._exec("DECLARE x; RETURN coalesce(x, 0);") == 0
        assert self._exec("RETURN coalesce(42, 0);") == 42

    def test_builtin_str_int_float(self):
        assert self._exec("RETURN str(42);") == "42"
        assert self._exec("RETURN int(3.7);") == 3
        assert self._exec("RETURN float(3);") == 3.0

    def test_builtin_range(self):
        src = """
        DECLARE r = range(3);
        RETURN len(r);
        """
        assert self._exec(src) == 3

    def test_builtin_now(self):
        result = self._exec("RETURN now();")
        assert isinstance(result, float)
        assert result > 0

    # Custom builtins
    def test_register_builtin(self):
        from qm_core.procedures.plqm import PLQMInterpreter
        interp = PLQMInterpreter()
        interp.register_builtin("double", lambda x: x * 2)
        result = interp.execute("RETURN double(21);")
        assert result == 42

    # RAISE
    def test_raise_error(self):
        from qm_core.procedures.plqm import PLQMError
        with pytest.raises(PLQMError, match="something went wrong"):
            self._exec("RAISE 'something went wrong';")

    # Implicit return
    def test_no_return_gives_none(self):
        assert self._exec("DECLARE x = 1;") is None

    # Dot access
    def test_dot_access_dict(self):
        src = "RETURN obj.name;"
        assert self._exec(src, {"obj": {"name": "Alice"}}) == "Alice"

    # Line comments
    def test_comments(self):
        src = """
        -- This is a comment
        DECLARE x = 10;  -- inline comment
        RETURN x;
        """
        assert self._exec(src) == 10

    # Nested control flow
    def test_nested_if_while(self):
        src = """
        DECLARE sum = 0;
        DECLARE i = 0;
        WHILE i < 10 DO
            IF i % 2 = 0 THEN
                SET sum = sum + i;
            END IF;
            SET i = i + 1;
        END WHILE;
        RETURN sum;
        """
        assert self._exec(src) == 20  # 0+2+4+6+8

    # Undefined variable
    def test_undefined_variable(self):
        from qm_core.procedures.plqm import PLQMError
        with pytest.raises(PLQMError, match="Undefined variable"):
            self._exec("RETURN xyz;")

    # Undefined function
    def test_undefined_function(self):
        from qm_core.procedures.plqm import PLQMError
        with pytest.raises(PLQMError, match="Undefined function"):
            self._exec("RETURN unknown_fn(1);")


# ─── 2. Procedure Catalog ──────────────────────────────────────────

class TestProcedureCatalog:
    """Stored procedure registration & execution."""

    def _make_catalog(self):
        from qm_core.procedures.catalog import ProcedureCatalog
        return ProcedureCatalog()

    def _make_proc(self, name="add_nums", body="RETURN a + b;", params=None):
        from qm_core.procedures.catalog import StoredProcedure, ProcedureParam, ParamType
        if params is None:
            params = [
                ProcedureParam("a", ParamType.INT),
                ProcedureParam("b", ParamType.INT),
            ]
        return StoredProcedure(name=name, params=params, body=body)

    def test_register_and_call(self):
        cat = self._make_catalog()
        cat.register(self._make_proc())
        assert cat.call("add_nums", {"a": 3, "b": 4}) == 7

    def test_call_nonexistent(self):
        from qm_core.procedures.plqm import PLQMError
        cat = self._make_catalog()
        with pytest.raises(PLQMError, match="Procedure not found"):
            cat.call("nope")

    def test_drop(self):
        cat = self._make_catalog()
        cat.register(self._make_proc())
        assert cat.drop("add_nums") is True
        assert cat.get("add_nums") is None
        assert cat.drop("add_nums") is False

    def test_list_procedures(self):
        cat = self._make_catalog()
        cat.register(self._make_proc("p1", "RETURN 1;", params=[]))
        cat.register(self._make_proc("p2", "RETURN 2;", params=[]))
        assert len(cat.list_procedures()) == 2

    def test_case_insensitive_lookup(self):
        cat = self._make_catalog()
        cat.register(self._make_proc("MyProc", "RETURN 1;", params=[]))
        assert cat.get("myproc") is not None
        assert cat.call("MYPROC") == 1

    def test_missing_required_param(self):
        from qm_core.procedures.plqm import PLQMError
        cat = self._make_catalog()
        cat.register(self._make_proc())
        with pytest.raises(PLQMError, match="Missing required parameter"):
            cat.call("add_nums", {"a": 1})  # missing 'b'

    def test_default_param(self):
        from qm_core.procedures.catalog import StoredProcedure, ProcedureParam, ParamType
        cat = self._make_catalog()
        proc = StoredProcedure(
            name="greet",
            params=[
                ProcedureParam("name", ParamType.TEXT),
                ProcedureParam("prefix", ParamType.TEXT, default="Hello", required=False),
            ],
            body="RETURN prefix + ' ' + name;"
        )
        cat.register(proc)
        assert cat.call("greet", {"name": "Alice"}) == "Hello Alice"
        assert cat.call("greet", {"name": "Bob", "prefix": "Hi"}) == "Hi Bob"

    def test_type_coercion(self):
        from qm_core.procedures.catalog import StoredProcedure, ProcedureParam, ParamType
        cat = self._make_catalog()
        proc = StoredProcedure(
            name="double",
            params=[ProcedureParam("n", ParamType.INT)],
            body="RETURN n * 2;"
        )
        cat.register(proc)
        assert cat.call("double", {"n": "5"}) == 10

    def test_stats(self):
        cat = self._make_catalog()
        cat.register(self._make_proc())
        cat.call("add_nums", {"a": 1, "b": 2})
        cat.call("add_nums", {"a": 3, "b": 4})
        st = cat.stats()
        assert "add_nums" in st
        assert st["add_nums"]["exec_count"] == 2
        assert st["add_nums"]["total_time_ms"] >= 0

    def test_custom_builtin_from_catalog(self):
        """Register a builtin on the catalog's interpreter and use it inside a procedure."""
        from qm_core.procedures.catalog import StoredProcedure, ProcedureParam, ParamType
        cat = self._make_catalog()
        cat.interpreter.register_builtin("square", lambda x: x * x)
        proc = StoredProcedure(
            name="sq",
            params=[ProcedureParam("n", ParamType.INT)],
            body="RETURN square(n);"
        )
        cat.register(proc)
        assert cat.call("sq", {"n": 7}) == 49


# ─── 3. Trigger System ─────────────────────────────────────────────

class TestTriggerSystem:
    """BEFORE/AFTER triggers with cancellation, priority, and conditional predicates."""

    def _make_system(self):
        from qm_core.triggers.trigger import TriggerCatalog, TriggerExecutor
        cat = TriggerCatalog()
        exe = TriggerExecutor(cat)
        return cat, exe

    def _ctx(self, table="articles", event_name="insert", timing_name="before",
             new_row=None, old_row=None):
        from qm_core.triggers.trigger import TriggerContext, TriggerEvent, TriggerTiming
        event = TriggerEvent(event_name)
        timing = TriggerTiming(timing_name)
        return TriggerContext(
            table=table, event=event, timing=timing,
            new_row=dict(new_row) if new_row else None,
            old_row=dict(old_row) if old_row else None,
        )

    def _trigger(self, name, table, event, timing, action, priority=100, when=None):
        from qm_core.triggers.trigger import Trigger, TriggerEvent, TriggerTiming
        return Trigger(
            name=name, table=table,
            event=TriggerEvent(event), timing=TriggerTiming(timing),
            action=action, priority=priority, when=when,
        )

    # Basic BEFORE trigger
    def test_before_insert_modifies_row(self):
        cat, exe = self._make_system()

        def set_default(ctx):
            ctx.new_row["status"] = "draft"

        cat.register(self._trigger("auto_draft", "articles", "insert", "before", set_default))
        ctx = self._ctx(new_row={"title": "Hello"})
        exe.fire_before(ctx)

        assert ctx.new_row["status"] == "draft"
        assert not ctx.cancelled
        assert exe.fire_count == 1

    # BEFORE trigger cancels operation
    def test_before_trigger_cancels(self):
        cat, exe = self._make_system()

        def block_empty(ctx):
            if not ctx.new_row.get("title"):
                ctx.cancel()

        cat.register(self._trigger("block_empty", "articles", "insert", "before", block_empty))
        ctx = self._ctx(new_row={"title": ""})
        exe.fire_before(ctx)

        assert ctx.cancelled is True

    # AFTER trigger
    def test_after_insert(self):
        cat, exe = self._make_system()
        log = []

        def audit(ctx):
            log.append(f"inserted:{ctx.new_row.get('id')}")

        cat.register(self._trigger("audit_insert", "articles", "insert", "after", audit))
        ctx = self._ctx(new_row={"id": "a1"})
        ctx.timing = __import__("qm_core.triggers.trigger", fromlist=["TriggerTiming"]).TriggerTiming.AFTER
        exe.fire_after(ctx)

        assert log == ["inserted:a1"]

    # Priority ordering
    def test_trigger_priority_ordering(self):
        cat, exe = self._make_system()
        order = []

        cat.register(self._trigger("low", "t", "insert", "before",
                                   lambda ctx: order.append("low"), priority=50))
        cat.register(self._trigger("high", "t", "insert", "before",
                                   lambda ctx: order.append("high"), priority=10))
        cat.register(self._trigger("mid", "t", "insert", "before",
                                   lambda ctx: order.append("mid"), priority=30))

        ctx = self._ctx(table="t", new_row={})
        exe.fire_before(ctx)

        assert order == ["high", "mid", "low"]

    # Cancellation stops subsequent triggers
    def test_cancel_stops_chain(self):
        cat, exe = self._make_system()
        triggered = []

        cat.register(self._trigger("first", "t", "insert", "before",
                                   lambda ctx: ctx.cancel(), priority=10))
        cat.register(self._trigger("second", "t", "insert", "before",
                                   lambda ctx: triggered.append("second"), priority=20))

        ctx = self._ctx(table="t", new_row={})
        exe.fire_before(ctx)

        assert ctx.cancelled
        assert triggered == []  # second never fired

    # Conditional WHEN predicate
    def test_conditional_when(self):
        cat, exe = self._make_system()
        log = []

        cat.register(self._trigger(
            "log_update", "articles", "update", "after",
            lambda ctx: log.append(ctx.new_row.get("id")),
            when=lambda ctx: ctx.new_row.get("status") == "published",
        ))

        ctx1 = self._ctx(event_name="update", timing_name="after",
                         new_row={"id": "a1", "status": "draft"})
        exe.fire_after(ctx1)

        ctx2 = self._ctx(event_name="update", timing_name="after",
                         new_row={"id": "a2", "status": "published"})
        exe.fire_after(ctx2)

        assert log == ["a2"]

    # Enable/disable
    def test_enable_disable(self):
        cat, exe = self._make_system()
        log = []

        cat.register(self._trigger("tr", "t", "insert", "before",
                                   lambda ctx: log.append("fired")))
        cat.disable("tr")

        exe.fire_before(self._ctx(table="t", new_row={}))
        assert log == []

        cat.enable("tr")
        exe.fire_before(self._ctx(table="t", new_row={}))
        assert log == ["fired"]

    # Drop trigger
    def test_drop_trigger(self):
        cat, _ = self._make_system()
        cat.register(self._trigger("tr", "t", "insert", "before", lambda ctx: None))
        assert cat.drop("tr") is True
        assert cat.get("tr") is None
        assert cat.drop("tr") is False

    # List triggers
    def test_list_triggers(self):
        cat, _ = self._make_system()
        cat.register(self._trigger("a", "t1", "insert", "before", lambda ctx: None))
        cat.register(self._trigger("b", "t2", "update", "after", lambda ctx: None))
        cat.register(self._trigger("c", "t1", "delete", "before", lambda ctx: None))

        assert len(cat.list_triggers()) == 3
        assert len(cat.list_triggers("t1")) == 2

    # Fire log
    def test_fire_log(self):
        cat, exe = self._make_system()
        cat.register(self._trigger("tr", "t", "insert", "before", lambda ctx: None))
        exe.fire_before(self._ctx(table="t", new_row={}))

        log = exe.fire_log
        assert len(log) == 1
        assert log[0]["trigger"] == "tr"
        assert log[0]["timing"] == "before"

    # DELETE trigger with old_row
    def test_before_delete_trigger(self):
        cat, exe = self._make_system()
        captured = {}

        def capture_delete(ctx):
            captured["old_id"] = ctx.old_row.get("id")

        cat.register(self._trigger("cap_del", "t", "delete", "before", capture_delete))
        ctx = self._ctx(table="t", event_name="delete", old_row={"id": "x1"})
        exe.fire_before(ctx)

        assert captured["old_id"] == "x1"


# ─── 4. Event Bus ──────────────────────────────────────────────────

class TestEventBus:
    """Pub/sub event bus for database change events."""

    def _make_bus(self):
        from qm_core.events.bus import EventBus
        return EventBus()

    def _event(self, event_type_str="row_inserted", table="articles", key="a1", data=None):
        from qm_core.events.bus import Event, EventType
        return Event(
            event_type=EventType(event_type_str),
            table=table,
            key=key,
            data=data or {},
        )

    def test_subscribe_and_publish(self):
        bus = self._make_bus()
        received = []
        bus.subscribe(callback=lambda e: received.append(e.key))
        bus.publish(self._event(key="a1"))
        assert received == ["a1"]

    def test_filter_by_event_type(self):
        from qm_core.events.bus import EventType
        bus = self._make_bus()
        received = []
        bus.subscribe(callback=lambda e: received.append(e.key),
                      event_types={EventType.ROW_DELETED})
        bus.publish(self._event("row_inserted", key="a1"))
        bus.publish(self._event("row_deleted", key="a2"))
        assert received == ["a2"]

    def test_filter_by_table(self):
        bus = self._make_bus()
        received = []
        bus.subscribe(callback=lambda e: received.append(e.key),
                      tables={"users"})
        bus.publish(self._event(table="articles", key="a1"))
        bus.publish(self._event(table="users", key="u1"))
        assert received == ["u1"]

    def test_custom_filter_fn(self):
        bus = self._make_bus()
        received = []
        bus.subscribe(
            callback=lambda e: received.append(e.key),
            filter_fn=lambda e: e.data.get("priority") == "high",
        )
        bus.publish(self._event(key="a1", data={"priority": "low"}))
        bus.publish(self._event(key="a2", data={"priority": "high"}))
        assert received == ["a2"]

    def test_unsubscribe(self):
        bus = self._make_bus()
        received = []
        sub_id = bus.subscribe(callback=lambda e: received.append(e.key))
        bus.publish(self._event(key="a1"))
        bus.unsubscribe(sub_id)
        bus.publish(self._event(key="a2"))
        assert received == ["a1"]

    def test_multiple_subscribers(self):
        bus = self._make_bus()
        r1, r2 = [], []
        bus.subscribe(callback=lambda e: r1.append(e.key))
        bus.subscribe(callback=lambda e: r2.append(e.key))
        bus.publish(self._event(key="x"))
        assert r1 == ["x"]
        assert r2 == ["x"]

    def test_publish_returns_notified_count(self):
        bus = self._make_bus()
        bus.subscribe(callback=lambda e: None)
        bus.subscribe(callback=lambda e: None)
        count = bus.publish(self._event())
        assert count == 2

    def test_event_history(self):
        bus = self._make_bus()
        bus.publish(self._event(key="a1"))
        bus.publish(self._event(key="a2"))
        hist = bus.get_history()
        assert len(hist) == 2
        assert hist[0].key == "a1"

    def test_history_filtered_by_type(self):
        from qm_core.events.bus import EventType
        bus = self._make_bus()
        bus.publish(self._event("row_inserted", key="a1"))
        bus.publish(self._event("row_deleted", key="a2"))
        hist = bus.get_history(event_type=EventType.ROW_DELETED)
        assert len(hist) == 1
        assert hist[0].key == "a2"

    def test_history_size_limit(self):
        from qm_core.events.bus import EventBus
        bus = EventBus(history_size=5)
        for i in range(10):
            bus.publish(self._event(key=str(i)))
        assert len(bus.get_history()) == 5

    def test_publish_count(self):
        bus = self._make_bus()
        bus.publish(self._event())
        bus.publish(self._event())
        assert bus.publish_count == 2

    def test_subscriber_count(self):
        bus = self._make_bus()
        bus.subscribe(callback=lambda e: None)
        bus.subscribe(callback=lambda e: None)
        assert bus.subscriber_count == 2

    def test_event_topic(self):
        ev = self._event("row_inserted", table="articles")
        assert ev.topic == "articles.row_inserted"


# ─── 5. Event Pipeline ─────────────────────────────────────────────

class TestEventPipeline:
    """Composable filter → transform → sink chains."""

    def _make_pipeline(self, name="test"):
        from qm_core.events.pipeline import EventPipeline
        return EventPipeline(name)

    def _event(self, event_type_str="row_inserted", table="articles", key="a1", data=None):
        from qm_core.events.bus import Event, EventType
        return Event(
            event_type=EventType(event_type_str),
            table=table, key=key,
            data=data or {},
        )

    def test_passthrough(self):
        """No stages → event passes through."""
        pipe = self._make_pipeline()
        result = pipe.process(self._event(key="a1"))
        assert result.key == "a1"
        assert pipe.processed == 1

    def test_filter_drops(self):
        pipe = self._make_pipeline()
        pipe.filter(lambda e: e.table == "users")
        result = pipe.process(self._event(table="articles"))
        assert result is None
        assert pipe.dropped == 1
        assert pipe.processed == 0

    def test_filter_passes(self):
        pipe = self._make_pipeline()
        pipe.filter(lambda e: e.table == "articles")
        result = pipe.process(self._event(table="articles", key="a1"))
        assert result.key == "a1"
        assert pipe.processed == 1

    def test_transform(self):
        pipe = self._make_pipeline()
        pipe.transform(lambda e: {"id": e.key, "action": e.event_type.value})
        result = pipe.process(self._event(key="a1"))
        assert result == {"id": "a1", "action": "row_inserted"}

    def test_sink(self):
        collected = []
        pipe = self._make_pipeline()
        pipe.transform(lambda e: {"k": e.key})
        pipe.sink(lambda rec: collected.append(rec))
        pipe.process(self._event(key="a1"))
        assert collected == [{"k": "a1"}]

    def test_full_chain(self):
        """filter → transform → sink pipeline."""
        results = []
        pipe = self._make_pipeline()
        pipe.filter(lambda e: e.data.get("status") == "published")
        pipe.transform(lambda e: {"id": e.key, "title": e.data.get("title")})
        pipe.sink(lambda rec: results.append(rec))

        pipe.process(self._event(key="a1", data={"status": "draft", "title": "D"}))
        pipe.process(self._event(key="a2", data={"status": "published", "title": "P"}))

        assert len(results) == 1
        assert results[0] == {"id": "a2", "title": "P"}
        assert pipe.dropped == 1
        assert pipe.processed == 1

    def test_chained_builder(self):
        """Builder methods return self for chaining."""
        pipe = self._make_pipeline()
        result = pipe.filter(lambda e: True).transform(lambda e: e).sink(lambda r: None)
        assert result is pipe

    def test_attach_to_bus(self):
        from qm_core.events.bus import EventBus
        bus = EventBus()
        received = []

        pipe = self._make_pipeline()
        pipe.transform(lambda e: e.key)
        pipe.sink(lambda k: received.append(k))
        pipe.attach(bus)

        assert pipe.is_attached

        from qm_core.events.bus import Event, EventType
        bus.publish(Event(event_type=EventType.ROW_INSERTED, table="t", key="x"))

        assert received == ["x"]
        assert pipe.processed == 1

    def test_detach(self):
        from qm_core.events.bus import EventBus, Event, EventType
        bus = EventBus()
        received = []

        pipe = self._make_pipeline()
        pipe.sink(lambda e: received.append(e.key))
        pipe.attach(bus)
        bus.publish(Event(event_type=EventType.ROW_INSERTED, table="t", key="x1"))
        pipe.detach()
        bus.publish(Event(event_type=EventType.ROW_INSERTED, table="t", key="x2"))

        assert not pipe.is_attached
        assert received == ["x1"]

    def test_pipeline_error_captured(self):
        from qm_core.events.bus import EventBus, Event, EventType
        bus = EventBus()

        pipe = self._make_pipeline()
        pipe.transform(lambda e: 1 / 0)  # Will raise ZeroDivisionError
        pipe.attach(bus)

        bus.publish(Event(event_type=EventType.ROW_INSERTED, table="t", key="x"))

        assert len(pipe.errors) == 1
        assert isinstance(pipe.errors[0][1], ZeroDivisionError)

    def test_stages_introspection(self):
        from qm_core.events.pipeline import StageKind
        pipe = self._make_pipeline()
        pipe.filter(lambda e: True, name="my_filter")
        pipe.transform(lambda e: e, name="my_transform")
        pipe.sink(lambda r: None, name="my_sink")

        stages = pipe.stages
        assert len(stages) == 3
        assert stages[0].kind == StageKind.FILTER
        assert stages[0].name == "my_filter"
        assert stages[1].kind == StageKind.TRANSFORM
        assert stages[2].kind == StageKind.SINK

    def test_multiple_filters(self):
        pipe = self._make_pipeline()
        pipe.filter(lambda e: e.table == "articles")
        pipe.filter(lambda e: e.data.get("published", False))

        assert pipe.process(self._event(table="articles", data={"published": True})) is not None
        assert pipe.process(self._event(table="articles", data={"published": False})) is None
        assert pipe.process(self._event(table="users", data={"published": True})) is None

    def test_multiple_transforms(self):
        pipe = self._make_pipeline()
        pipe.transform(lambda e: {"key": e.key})
        pipe.transform(lambda d: {**d, "extra": True})

        result = pipe.process(self._event(key="a1"))
        assert result == {"key": "a1", "extra": True}


# ─── 6. Integration: Triggers + Events ─────────────────────────────

class TestTriggerEventIntegration:
    """Trigger firing publishes events to the event bus."""

    def test_trigger_publishes_event(self):
        from qm_core.triggers.trigger import (
            TriggerCatalog, TriggerExecutor, TriggerContext,
            Trigger, TriggerEvent, TriggerTiming,
        )
        from qm_core.events.bus import EventBus, Event, EventType

        bus = EventBus()
        cat = TriggerCatalog()
        exe = TriggerExecutor(cat)

        received = []
        bus.subscribe(callback=lambda e: received.append(e))

        # After-insert trigger that publishes to the event bus
        def publish_event(ctx):
            bus.publish(Event(
                event_type=EventType.ROW_INSERTED,
                table=ctx.table,
                key=ctx.new_row.get("id", ""),
                data=dict(ctx.new_row),
            ))

        cat.register(Trigger(
            name="publish_insert",
            table="articles",
            event=TriggerEvent.INSERT,
            timing=TriggerTiming.AFTER,
            action=publish_event,
        ))

        ctx = TriggerContext(
            table="articles",
            event=TriggerEvent.INSERT,
            timing=TriggerTiming.AFTER,
            new_row={"id": "a1", "title": "Test"},
        )
        exe.fire_after(ctx)

        assert len(received) == 1
        assert received[0].table == "articles"
        assert received[0].key == "a1"

    def test_trigger_event_pipeline_e2e(self):
        """Full end-to-end: trigger → event bus → pipeline → sink."""
        from qm_core.triggers.trigger import (
            TriggerCatalog, TriggerExecutor, TriggerContext,
            Trigger, TriggerEvent, TriggerTiming,
        )
        from qm_core.events.bus import EventBus, Event, EventType
        from qm_core.events.pipeline import EventPipeline

        sink_data = []
        bus = EventBus()

        # Build pipeline: filter published articles → extract title → collect
        pipe = EventPipeline("cdc")
        pipe.filter(lambda e: e.data.get("status") == "published")
        pipe.transform(lambda e: {"title": e.data.get("title")})
        pipe.sink(lambda rec: sink_data.append(rec))
        pipe.attach(bus)

        # Trigger that publishes to bus
        cat = TriggerCatalog()
        exe = TriggerExecutor(cat)

        def on_insert(ctx):
            bus.publish(Event(
                event_type=EventType.ROW_INSERTED,
                table=ctx.table,
                data=dict(ctx.new_row),
            ))

        cat.register(Trigger(
            name="cdc_trigger",
            table="articles",
            event=TriggerEvent.INSERT,
            timing=TriggerTiming.AFTER,
            action=on_insert,
        ))

        # Simulate two inserts
        for row in [
            {"title": "Draft", "status": "draft"},
            {"title": "Published", "status": "published"},
        ]:
            ctx = TriggerContext(
                table="articles", event=TriggerEvent.INSERT,
                timing=TriggerTiming.AFTER, new_row=row,
            )
            exe.fire_after(ctx)

        assert len(sink_data) == 1
        assert sink_data[0] == {"title": "Published"}
        pipe.detach()


# ─── 7. Module Import Sanity ───────────────────────────────────────

class TestPhase11Imports:
    """Verify all Phase 11 public APIs are importable."""

    def test_import_procedures(self):
        from qm_core.procedures import PLQMInterpreter, PLQMError, ProcedureCatalog, StoredProcedure, ProcedureParam
        assert PLQMInterpreter is not None
        assert ProcedureCatalog is not None

    def test_import_triggers(self):
        from qm_core.triggers import (
            Trigger, TriggerEvent, TriggerTiming, TriggerLevel,
            TriggerCatalog, TriggerExecutor, TriggerContext,
        )
        assert TriggerCatalog is not None
        assert TriggerExecutor is not None

    def test_import_events(self):
        from qm_core.events import EventBus, Event, EventType, EventPipeline, PipelineStage
        assert EventBus is not None
        assert EventPipeline is not None

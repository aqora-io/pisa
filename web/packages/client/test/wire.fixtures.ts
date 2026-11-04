// Written by crates/pisa-protocol/tests/ts_fixtures.rs from the Rust types: don't
// edit. After changing them, run
// `PISA_UPDATE_FIXTURES=1 cargo test -p pisa-protocol --test ts_fixtures`.
import type * as T from "../src/types.ts";

const checkResultFull = {
  "declarations": [
    {
      "axioms": [
        "propext"
      ],
      "kind": "theorem",
      "name": "t",
      "type_hash": "ab",
      "value_hash": "cd"
    }
  ],
  "error": {
    "kind": "lean",
    "message": "failed"
  },
  "id": "a",
  "messages": [
    {
      "data": "Nat : Type",
      "end_pos": {
        "column": 2,
        "line": 1
      },
      "pos": {
        "column": 2,
        "line": 1
      },
      "severity": "info"
    }
  ],
  "ok": true,
  "reasons": [
    "axiom: t uses sorryAx"
  ],
  "sorries": [
    {
      "end_pos": {
        "column": 2,
        "line": 1
      },
      "goal": "⊢ True",
      "pos": {
        "column": 2,
        "line": 1
      }
    }
  ],
  "tactics": [
    {
      "end_pos": {
        "column": 2,
        "line": 1
      },
      "goals_after": [],
      "goals_before": [
        "⊢ True"
      ],
      "pos": {
        "column": 2,
        "line": 1
      },
      "tactic": "simp"
    }
  ],
  "time": 0.25
} satisfies T.CheckResult;

const batchResponseFull = {
  "results": [
    {
      "declarations": [
        {
          "axioms": [
            "propext"
          ],
          "kind": "theorem",
          "name": "t",
          "type_hash": "ab",
          "value_hash": "cd"
        }
      ],
      "error": {
        "kind": "lean",
        "message": "failed"
      },
      "id": "a",
      "messages": [
        {
          "data": "Nat : Type",
          "end_pos": {
            "column": 2,
            "line": 1
          },
          "pos": {
            "column": 2,
            "line": 1
          },
          "severity": "info"
        }
      ],
      "ok": true,
      "reasons": [
        "axiom: t uses sorryAx"
      ],
      "sorries": [
        {
          "end_pos": {
            "column": 2,
            "line": 1
          },
          "goal": "⊢ True",
          "pos": {
            "column": 2,
            "line": 1
          }
        }
      ],
      "tactics": [
        {
          "end_pos": {
            "column": 2,
            "line": 1
          },
          "goals_after": [],
          "goals_before": [
            "⊢ True"
          ],
          "pos": {
            "column": 2,
            "line": 1
          },
          "tactic": "simp"
        }
      ],
      "time": 0.25
    }
  ]
} satisfies T.BatchResponse;

const startResponseFull = {
  "messages": [
    {
      "data": "Nat : Type",
      "end_pos": {
        "column": 2,
        "line": 1
      },
      "pos": {
        "column": 2,
        "line": 1
      },
      "severity": "info"
    }
  ],
  "session": "v~h~id",
  "states": [
    {
      "end_pos": {
        "column": 2,
        "line": 1
      },
      "goal": "⊢ True",
      "pos": {
        "column": 2,
        "line": 1
      },
      "state": "v~h~s"
    }
  ]
} satisfies T.StartResponse;

const tacticResponseFull = {
  "complete": true,
  "goals": [],
  "messages": [
    {
      "data": "Nat : Type",
      "end_pos": {
        "column": 2,
        "line": 1
      },
      "pos": {
        "column": 2,
        "line": 1
      },
      "severity": "info"
    }
  ],
  "state": "v~h~s2"
} satisfies T.TacticResponse;

const snapshotResponseFull = {
  "dirty": false,
  "entry": "e",
  "header": "import PisaSnap.s0",
  "labels": {
    "run": "7"
  },
  "result": {
    "declarations": [
      {
        "axioms": [
          "propext"
        ],
        "kind": "theorem",
        "name": "t",
        "type_hash": "ab",
        "value_hash": "cd"
      }
    ],
    "error": {
      "kind": "lean",
      "message": "failed"
    },
    "id": "a",
    "messages": [
      {
        "data": "Nat : Type",
        "end_pos": {
          "column": 2,
          "line": 1
        },
        "pos": {
          "column": 2,
          "line": 1
        },
        "severity": "info"
      }
    ],
    "ok": true,
    "reasons": [
      "axiom: t uses sorryAx"
    ],
    "sorries": [
      {
        "end_pos": {
          "column": 2,
          "line": 1
        },
        "goal": "⊢ True",
        "pos": {
          "column": 2,
          "line": 1
        }
      }
    ],
    "tactics": [
      {
        "end_pos": {
          "column": 2,
          "line": 1
        },
        "goals_after": [],
        "goals_before": [
          "⊢ True"
        ],
        "pos": {
          "column": 2,
          "line": 1
        },
        "tactic": "simp"
      }
    ],
    "time": 0.25
  },
  "snapshot": "PisaSnap.s0",
  "verified": true,
  "version": "v"
} satisfies T.SnapshotResponse;

const snapshotInfoFull = {
  "code": "def x := 1",
  "created": 1,
  "dirty": false,
  "entry": "e",
  "header": "import PisaSnap.s0",
  "imports": [
    "PisaSnap.s1"
  ],
  "labels": {
    "run": "7"
  },
  "result": {
    "declarations": [
      {
        "axioms": [
          "propext"
        ],
        "kind": "theorem",
        "name": "t",
        "type_hash": "ab",
        "value_hash": "cd"
      }
    ],
    "error": {
      "kind": "lean",
      "message": "failed"
    },
    "id": "a",
    "messages": [
      {
        "data": "Nat : Type",
        "end_pos": {
          "column": 2,
          "line": 1
        },
        "pos": {
          "column": 2,
          "line": 1
        },
        "severity": "info"
      }
    ],
    "ok": true,
    "reasons": [
      "axiom: t uses sorryAx"
    ],
    "sorries": [
      {
        "end_pos": {
          "column": 2,
          "line": 1
        },
        "goal": "⊢ True",
        "pos": {
          "column": 2,
          "line": 1
        }
      }
    ],
    "tactics": [
      {
        "end_pos": {
          "column": 2,
          "line": 1
        },
        "goals_after": [],
        "goals_before": [
          "⊢ True"
        ],
        "pos": {
          "column": 2,
          "line": 1
        },
        "tactic": "simp"
      }
    ],
    "time": 0.25
  },
  "snapshot": "PisaSnap.s0",
  "verified": true
} satisfies T.SnapshotInfo;

const snapshotListFull = {
  "entries": [
    {
      "created": 1,
      "dirty": false,
      "entry": "e",
      "header": "import PisaSnap.s0",
      "imports": [],
      "labels": {
        "run": "7"
      },
      "snapshot": "PisaSnap.s0",
      "verified": true
    }
  ],
  "next": "c"
} satisfies T.SnapshotList;

const snapshotVerifiedFull = {
  "checked": [
    "PisaSnap.s0"
  ],
  "dirty": false,
  "entry": "e",
  "failure": {
    "reason": "kernel",
    "snapshot": "PisaSnap.s0"
  },
  "snapshot": "PisaSnap.s0",
  "verified": false
} satisfies T.SnapshotVerified;

const searchResponseFull = {
  "env_id": "e",
  "results": [
    {
      "hits": [
        {
          "flags": {
            "additive_sibling": "add",
            "deprecated_to": "New",
            "imports": "import Mathlib",
            "informal": true,
            "uses_sorry": true
          },
          "informal_name": "commutativity",
          "kind": "theorem",
          "library": "Mathlib",
          "module": "Mathlib.Algebra",
          "name": "Nat.add_comm",
          "score": 1.5,
          "signature": "Nat.add_comm (n m : ℕ) : n + m = m + n",
          "snapshot": {
            "dirty": false,
            "entries": [
              "e"
            ],
            "verified": true
          },
          "source_url": "https://example.com",
          "summary": "Addition commutes.",
          "text": "Nat.add_comm …"
        }
      ],
      "verification": "unavailable",
      "warnings": [
        "slow"
      ]
    }
  ]
} satisfies T.SearchResponse;

const resolveResponseFull = {
  "env_id": "e",
  "results": [
    {
      "deprecated_to": "y",
      "exists": false,
      "name": "x",
      "suggestions": [
        {
          "kind": "def",
          "name": "y",
          "signature": "y : Nat"
        }
      ]
    }
  ]
} satisfies T.ResolveResponse;

const declResponseFull = {
  "decl": {
    "deps": [
      "Nat"
    ],
    "docstring": "The x.",
    "flags": {
      "additive_sibling": "add",
      "deprecated_to": "New",
      "imports": "import Mathlib",
      "informal": true,
      "uses_sorry": true
    },
    "informal": {
      "aliases": [
        "ex"
      ],
      "informal_name": "x",
      "source": "llm",
      "statement_latex": "x",
      "statement_nl": "x"
    },
    "kind": "def",
    "library": "Mathlib",
    "module": "Mathlib.X",
    "name": "x",
    "signature": "x : Nat",
    "source_url": "https://example.com",
    "type_full": "Nat",
    "used_by": []
  },
  "env_id": "e"
} satisfies T.DeclResponse;

const checkRequestFull = {
  "code": "x",
  "detail": [
    "tactics"
  ],
  "timeout": 60,
  "version": "v"
} satisfies T.CheckRequest;

const batchRequestFull = {
  "detail": [
    "tactics"
  ],
  "items": [
    {
      "code": "x",
      "id": "a"
    }
  ],
  "timeout": 60,
  "version": "v"
} satisfies T.BatchRequest;

const startRequestFull = {
  "code": "x",
  "timeout": 60,
  "version": "v"
} satisfies T.StartRequest;

const snapshotRequestFull = {
  "code": "x",
  "key": "k",
  "labels": {
    "run": "7"
  },
  "timeout": 60,
  "verify": true,
  "version": "v"
} satisfies T.SnapshotRequest;

const labelsRequestFull = {
  "remove": [
    "old"
  ],
  "set": {
    "run": "7"
  }
} satisfies T.LabelsRequest;

const searchRequestFull = {
  "env_id": "e",
  "queries": [
    {
      "goal": "goal",
      "include_sorried": true,
      "k": 5,
      "kinds": [
        "theorem"
      ],
      "latex": "latex",
      "libraries": [
        "Mathlib"
      ],
      "name": "name",
      "nl": "nl",
      "pattern": "pattern",
      "q": "q",
      "verify": true
    },
    {
      "goal": "goal",
      "include_sorried": true,
      "k": 5,
      "kinds": [
        "theorem"
      ],
      "latex": "latex",
      "libraries": [
        "Mathlib"
      ],
      "name": "name",
      "nl": "nl",
      "pattern": "pattern",
      "q": "q",
      "snapshots": "all",
      "verify": true
    },
    {
      "goal": "goal",
      "include_sorried": true,
      "k": 5,
      "kinds": [
        "theorem"
      ],
      "latex": "latex",
      "libraries": [
        "Mathlib"
      ],
      "name": "name",
      "nl": "nl",
      "pattern": "pattern",
      "q": "q",
      "snapshots": {
        "labels": {
          "run": "7"
        }
      },
      "verify": true
    }
  ]
} satisfies T.SearchRequest;

const resolveRequestFull = {
  "env_id": "e",
  "names": [
    "x"
  ]
} satisfies T.ResolveRequest;

const checkResultEmpty = {
  "declarations": [
    {
      "axioms": [
        "propext"
      ],
      "kind": "theorem",
      "name": "t",
      "type_hash": "ab"
    }
  ],
  "messages": [
    {
      "data": "Nat : Type",
      "pos": {
        "column": 2,
        "line": 1
      },
      "severity": "info"
    }
  ],
  "ok": false,
  "reasons": [
    "axiom: t uses sorryAx"
  ],
  "sorries": [
    {
      "goal": "⊢ True"
    }
  ],
  "time": 0.25
} satisfies T.CheckResult;

const batchResponseEmpty = {
  "results": [
    {
      "declarations": [
        {
          "axioms": [
            "propext"
          ],
          "kind": "theorem",
          "name": "t",
          "type_hash": "ab"
        }
      ],
      "messages": [
        {
          "data": "Nat : Type",
          "pos": {
            "column": 2,
            "line": 1
          },
          "severity": "info"
        }
      ],
      "ok": false,
      "reasons": [
        "axiom: t uses sorryAx"
      ],
      "sorries": [
        {
          "goal": "⊢ True"
        }
      ],
      "time": 0.25
    }
  ]
} satisfies T.BatchResponse;

const startResponseEmpty = {
  "messages": [
    {
      "data": "Nat : Type",
      "pos": {
        "column": 2,
        "line": 1
      },
      "severity": "info"
    }
  ],
  "session": "v~h~id",
  "states": [
    {
      "goal": "⊢ True",
      "state": "v~h~s"
    }
  ]
} satisfies T.StartResponse;

const tacticResponseEmpty = {
  "complete": false,
  "goals": [],
  "messages": [
    {
      "data": "Nat : Type",
      "pos": {
        "column": 2,
        "line": 1
      },
      "severity": "info"
    }
  ],
  "state": "v~h~s2"
} satisfies T.TacticResponse;

const snapshotResponseEmpty = {
  "dirty": true,
  "result": {
    "declarations": [
      {
        "axioms": [
          "propext"
        ],
        "kind": "theorem",
        "name": "t",
        "type_hash": "ab"
      }
    ],
    "messages": [
      {
        "data": "Nat : Type",
        "pos": {
          "column": 2,
          "line": 1
        },
        "severity": "info"
      }
    ],
    "ok": false,
    "reasons": [
      "axiom: t uses sorryAx"
    ],
    "sorries": [
      {
        "goal": "⊢ True"
      }
    ],
    "time": 0.25
  },
  "verified": false,
  "version": "v"
} satisfies T.SnapshotResponse;

const snapshotInfoEmpty = {
  "code": "def x := 1",
  "created": 1,
  "dirty": false,
  "entry": "e",
  "header": "import PisaSnap.s0",
  "imports": [
    "PisaSnap.s1"
  ],
  "result": {
    "declarations": [
      {
        "axioms": [
          "propext"
        ],
        "kind": "theorem",
        "name": "t",
        "type_hash": "ab"
      }
    ],
    "messages": [
      {
        "data": "Nat : Type",
        "pos": {
          "column": 2,
          "line": 1
        },
        "severity": "info"
      }
    ],
    "ok": false,
    "reasons": [
      "axiom: t uses sorryAx"
    ],
    "sorries": [
      {
        "goal": "⊢ True"
      }
    ],
    "time": 0.25
  },
  "snapshot": "PisaSnap.s0",
  "verified": true
} satisfies T.SnapshotInfo;

const snapshotListEmpty = {
  "entries": [
    {
      "created": 1,
      "dirty": false,
      "entry": "e",
      "header": "import PisaSnap.s0",
      "imports": [],
      "labels": {},
      "snapshot": "PisaSnap.s0",
      "verified": false
    }
  ]
} satisfies T.SnapshotList;

const snapshotVerifiedEmpty = {
  "checked": [
    "PisaSnap.s0"
  ],
  "dirty": false,
  "entry": "e",
  "snapshot": "PisaSnap.s0",
  "verified": true
} satisfies T.SnapshotVerified;

const searchResponseEmpty = {
  "env_id": "e",
  "results": [
    {
      "hits": [
        {
          "flags": {},
          "kind": "theorem",
          "library": "Mathlib",
          "module": "Mathlib.Algebra",
          "name": "Nat.add_comm",
          "score": 1.5,
          "signature": "Nat.add_comm (n m : ℕ) : n + m = m + n",
          "text": "Nat.add_comm …"
        }
      ]
    }
  ]
} satisfies T.SearchResponse;

const resolveResponseEmpty = {
  "env_id": "e",
  "results": [
    {
      "exists": true,
      "name": "x"
    }
  ]
} satisfies T.ResolveResponse;

const declResponseEmpty = {
  "decl": {
    "deps": [
      "Nat"
    ],
    "flags": {},
    "kind": "def",
    "library": "Mathlib",
    "module": "Mathlib.X",
    "name": "x",
    "signature": "x : Nat",
    "type_full": "Nat",
    "used_by": []
  },
  "env_id": "e"
} satisfies T.DeclResponse;

const checkRequestEmpty = {
  "code": "x",
  "timeout": 60,
  "version": null
} satisfies T.CheckRequest;

const batchRequestEmpty = {
  "items": [
    {
      "code": "x",
      "id": "a"
    }
  ],
  "timeout": 60,
  "version": null
} satisfies T.BatchRequest;

const startRequestEmpty = {
  "code": "x",
  "timeout": 60,
  "version": null
} satisfies T.StartRequest;

const snapshotRequestEmpty = {
  "code": "x",
  "timeout": 60,
  "version": null
} satisfies T.SnapshotRequest;

const labelsRequestEmpty = {
  "remove": [],
  "set": {}
} satisfies T.LabelsRequest;

const searchRequestEmpty = {
  "queries": [
    {
      "include_sorried": false,
      "k": 5,
      "verify": false
    },
    {
      "include_sorried": false,
      "k": 5,
      "snapshots": "all",
      "verify": false
    },
    {
      "include_sorried": false,
      "k": 5,
      "snapshots": {
        "labels": {
          "run": "7"
        }
      },
      "verify": false
    }
  ]
} satisfies T.SearchRequest;

const resolveRequestEmpty = {
  "names": [
    "x"
  ]
} satisfies T.ResolveRequest;

const tacticRequest = {
  "tactic": "simp",
  "timeout": 60
} satisfies T.TacticRequest;

const sessionCheckRequest = {
  "code": "x",
  "timeout": 60
} satisfies T.SessionCheckRequest;

const deleted = {
  "entry": "e",
  "removed": [
    "PisaSnap.s0"
  ]
} satisfies T.Deleted;

const errorResponse = {
  "error": {
    "kind": "lean",
    "message": "failed"
  }
} satisfies T.ErrorResponse;

const errorKinds = ["timeout","out_of_memory","crash","policy","lean","capacity","unavailable","bad_request","not_found","internal"] as const satisfies readonly T.ErrorKind[];
const errorKindsComplete: Exclude<T.ErrorKind, (typeof errorKinds)[number]> extends never ? true : never = true;
const severities = ["trace","info","warning","error"] as const satisfies readonly T.Severity[];
const severitiesComplete: Exclude<T.Severity, (typeof severities)[number]> extends never ? true : never = true;

void [checkResultFull, batchResponseFull, startResponseFull, tacticResponseFull, snapshotResponseFull, snapshotInfoFull, snapshotListFull, snapshotVerifiedFull, searchResponseFull, resolveResponseFull, declResponseFull, checkRequestFull, batchRequestFull, startRequestFull, snapshotRequestFull, labelsRequestFull, searchRequestFull, resolveRequestFull, checkResultEmpty, batchResponseEmpty, startResponseEmpty, tacticResponseEmpty, snapshotResponseEmpty, snapshotInfoEmpty, snapshotListEmpty, snapshotVerifiedEmpty, searchResponseEmpty, resolveResponseEmpty, declResponseEmpty, checkRequestEmpty, batchRequestEmpty, startRequestEmpty, snapshotRequestEmpty, labelsRequestEmpty, searchRequestEmpty, resolveRequestEmpty, tacticRequest, sessionCheckRequest, deleted, errorResponse, errorKindsComplete, severitiesComplete];

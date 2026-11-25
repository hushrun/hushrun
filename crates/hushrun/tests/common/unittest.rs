//! The unittest adapter's fixtures, shared by its in-process tests (`units_unittest.rs`) and its
//! end-to-end ones (`e2e_unittest.rs`): the Python, Django, and the projects they run.
//!
//! The fixtures need Python 3.10 or later: `HUSHRUN_TEST_PYTHON` names it, `python3` by default;
//! the one that watches each test, 3.12 or later, and it skips below, as `reads_test`'s do. The
//! Django one needs a Python with Django: `HUSHRUN_TEST_DJANGO` names it. They say so and pass where
//! those are missing, unless `HUSHRUN_TEST_TOOLS=required`, which CI sets. No fixture needs the
//! network.

#![allow(dead_code)]

use super::repo::Repo;

/// Variables a unittest test's COMMAND never inherits besides [`super::repo::CLEARED`]: an outer
/// run's unittest session, pytest's options, a `PYTHONPATH` and Django's settings.
pub const UNITTEST_CLEARED: &[&str] = &[
    "HUSHRUN_UNITTEST_SESSION",
    "PYTEST_ADDOPTS",
    "PYTHONPATH",
    "DJANGO_SETTINGS_MODULE",
];

/// A repository of `files`, committed, whose COMMAND gets no outer unittest session, writes no
/// bytecode, and inherits none of [`UNITTEST_CLEARED`].
pub fn repo(files: &[(&str, String)]) -> Repo {
    let owned: Vec<(String, String)> = files
        .iter()
        .map(|(p, t)| (p.to_string(), t.clone()))
        .collect();
    Repo::new(&owned)
        .clear(UNITTEST_CLEARED)
        .give("PYTHONDONTWRITEBYTECODE", "1")
}

/// What one module read, as this machine's store keeps the check's map (ADR 0072).
pub fn reads(r: &Repo, unit: &str) -> Vec<String> {
    r.map()["units"][unit]["reads"]
        .as_array()
        .unwrap_or_else(|| panic!("{unit} has no reads"))
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect()
}

pub fn tools(what: &str, here: bool) -> bool {
    if here {
        return true;
    }
    if std::env::var("HUSHRUN_TEST_TOOLS").as_deref() == Ok("required") {
        panic!("{what} is missing, and HUSHRUN_TEST_TOOLS=required");
    }
    eprintln!("note: skipping: {what} is missing (HUSHRUN_TEST_TOOLS=required makes this fail)");
    false
}

pub fn python() -> String {
    std::env::var("HUSHRUN_TEST_PYTHON").unwrap_or_else(|_| "python3".into())
}

pub fn runs(python: &str, check: &str) -> bool {
    hushrun_testkit::tool::command(python)
        .args(["-c", check])
        .output()
        .is_ok_and(|o| o.status.success())
}

/// A Python 3.10 or later, said when missing. An older Python isn't a missing tool: the adapter
/// starts in none (it needs `sys.orig_argv`), and these skip.
pub fn with_python() -> Option<String> {
    let python = python();
    if !tools("Python (HUSHRUN_TEST_PYTHON)", runs(&python, "pass")) {
        return None;
    }
    if !runs(
        &python,
        "import sys; sys.exit(0 if sys.version_info >= (3, 10) else 1)",
    ) {
        eprintln!(
            "note: skipping: this Python is older than 3.10, where the adapter starts in none"
        );
        return None;
    }
    Some(python)
}

/// Whether `python` is 3.12 or later: it has `sys.monitoring`, and unittest's "no tests ran" is 5.
pub fn is_312(python: &str) -> bool {
    runs(
        python,
        "import sys; sys.exit(0 if sys.version_info >= (3, 12) else 1)",
    )
}

/// A Python with Django, said when missing.
pub fn with_django() -> Option<String> {
    let python = std::env::var("HUSHRUN_TEST_DJANGO").unwrap_or_else(|_| "python3".into());
    let here = runs(
        &python,
        "import sys, django; sys.exit(0 if sys.version_info >= (3, 10) else 1)",
    );
    tools(
        "a Python 3.10 or later with Django (HUSHRUN_TEST_DJANGO)",
        here,
    )
    .then_some(python)
}

pub fn yaml(tests: &str) -> String {
    format!("version: 1\nrepository: example.com/acme/shop\nchecks:\n  unit:\n    tests: {tests}\n")
}

/// A shop of unittest test modules: two modules each read by one test module, a greeting read by
/// five, one that starts a process, a module two imports away that only one test calls, and a
/// lockfile in the rest set.
pub fn shop(tests: &str) -> Vec<(&'static str, String)> {
    let mut files = vec![
        ("hushrun.yaml", yaml(tests)),
        (".gitignore", "__pycache__/\n".to_string()),
        ("requirements.txt", "nothing==1.0\n".to_string()),
        ("app/__init__.py", String::new()),
        ("app/pricing.py", "def price(x):\n    return x * 2\n".into()),
        (
            "app/cart.py",
            "from app.pricing import price\n\ndef total(xs):\n    return sum(price(x) for x in xs)\n"
                .into(),
        ),
        ("app/greet.py", "def hello():\n    return 'hi'\n".into()),
        ("app/deep.py", "def h():\n    return 1\n".into()),
        (
            "app/near.py",
            "from app import deep\n\ndef f():\n    return deep.h()\n".into(),
        ),
        ("app/other.py", "def g():\n    return 2\n".into()),
        (
            "app/facade.py",
            "from app import near, other\n\ndef fx():\n    return near.f()\n\ndef fy():\n    return other.g()\n".into(),
        ),
        ("tests/__init__.py", String::new()),
        (
            "tests/test_cart.py",
            "import unittest\nfrom app.cart import total\n\nclass CartTest(unittest.TestCase):\n    def test_total(self):\n        self.assertEqual(total([1, 2]), 6)\n".into(),
        ),
        (
            "tests/test_pricing.py",
            "import unittest\nfrom app.pricing import price\n\nclass PriceTest(unittest.TestCase):\n    def test_price(self):\n        self.assertEqual(price(3), 6)\n".into(),
        ),
        (
            "tests/test_child.py",
            "import subprocess, sys, unittest\n\nclass ChildTest(unittest.TestCase):\n    def test_child(self):\n        self.assertEqual(subprocess.run([sys.executable, '-c', 'pass']).returncode, 0)\n".into(),
        ),
        (
            "tests/test_split.py",
            "import unittest\nfrom app import facade\n\nclass Split(unittest.TestCase):\n    def test_near(self):\n        self.assertEqual(facade.fx(), 1)\n\n    def test_other(self):\n        self.assertEqual(facade.fy(), 2)\n".into(),
        ),
    ];
    for name in [
        "tests/test_greet_1.py",
        "tests/test_greet_2.py",
        "tests/test_greet_3.py",
        "tests/test_greet_4.py",
        "tests/test_greet_5.py",
    ] {
        files.push((
            name,
            "import unittest\nfrom app.greet import hello\n\nclass GreetTest(unittest.TestCase):\n    def test_hello(self):\n        self.assertEqual(hello(), 'hi')\n".into(),
        ));
    }
    files
}

/// A Django project of three apps: a shop with a model, a fixture, a template and a module only
/// it imports; a blog and news that import nothing of the shop's.
pub fn django_project() -> Vec<(&'static str, String)> {
    let settings = "SECRET_KEY = 'x'\nINSTALLED_APPS = ['django.contrib.contenttypes', 'django.contrib.auth', 'shop', 'blog', 'news']\nDATABASES = {'default': {'ENGINE': 'django.db.backends.sqlite3', 'NAME': 'db.sqlite3'}}\nTEMPLATES = [{'BACKEND': 'django.template.backends.django.DjangoTemplates', 'APP_DIRS': True}]\nUSE_TZ = True\nDEFAULT_AUTO_FIELD = 'django.db.models.AutoField'\n";
    let manage = "import os, sys\n\nif __name__ == '__main__':\n    os.environ.setdefault('DJANGO_SETTINGS_MODULE', 'proj.settings')\n    from django.core.management import execute_from_command_line\n    execute_from_command_line(sys.argv)\n";
    let simple = |class: &str| {
        format!(
            "from django.test import SimpleTestCase\n\nclass {class}(SimpleTestCase):\n    def test_it(self):\n        self.assertTrue(True)\n"
        )
    };
    vec![
        ("hushrun.yaml", yaml("reuse")),
        (".gitignore", "__pycache__/\ndb.sqlite3\n".into()),
        ("manage.py", manage.into()),
        ("proj/__init__.py", String::new()),
        ("proj/settings.py", settings.into()),
        ("shop/__init__.py", String::new()),
        (
            "shop/models.py",
            "from django.db import models\n\nclass Item(models.Model):\n    name = models.CharField(max_length=20)\n    price = models.IntegerField()\n".into(),
        ),
        ("shop/pricing.py", "def with_tax(p):\n    return p * 2\n".into()),
        ("shop/migrations/__init__.py", String::new()),
        (
            "shop/migrations/0001_initial.py",
            "from django.db import migrations, models\n\nclass Migration(migrations.Migration):\n    initial = True\n    dependencies = []\n    operations = [migrations.CreateModel(name='Item', fields=[('id', models.AutoField(primary_key=True)), ('name', models.CharField(max_length=20)), ('price', models.IntegerField())])]\n".into(),
        ),
        (
            "shop/fixtures/items.json",
            "[{\"model\": \"shop.item\", \"pk\": 1, \"fields\": {\"name\": \"a\", \"price\": 3}}]\n"
                .into(),
        ),
        ("shop/templates/shop/item.txt", "Item {{ item.name }}\n".into()),
        (
            "shop/tests.py",
            "from django.template.loader import render_to_string\nfrom django.test import TestCase\nfrom shop.models import Item\nfrom shop import pricing\n\nclass ItemTest(TestCase):\n    fixtures = ['items.json']\n\n    def test_fixture(self):\n        self.assertEqual(pricing.with_tax(Item.objects.get(pk=1).price), 6)\n\n    def test_render(self):\n        self.assertEqual(render_to_string('shop/item.txt', {'item': Item.objects.get(pk=1)}).strip(), 'Item a')\n".into(),
        ),
        ("blog/__init__.py", String::new()),
        ("blog/tests.py", simple("BlogTest")),
        ("news/__init__.py", String::new()),
        ("news/tests.py", simple("NewsTest")),
    ]
}

/// [`shop`], in `{mode: reuse, unit: test}`, with a class whose `tearDownClass` fails beside one
/// that passes, a module that fails to load, and one that skips itself as it loads.
pub fn teardown_shop() -> Vec<(&'static str, String)> {
    let mut files = shop("{mode: reuse, unit: test}");
    files.push((
        "tests/test_teardown.py",
        "import unittest\n\nclass Leaks(unittest.TestCase):\n    @classmethod\n    def tearDownClass(cls):\n        raise RuntimeError('leaked')\n\n    def test_a(self):\n        pass\n\nclass Clean(unittest.TestCase):\n    def test_b(self):\n        pass\n".into(),
    ));
    files.push((
        "tests/test_broken.py",
        "import no_such_module_here\n".into(),
    ));
    files.push((
        "tests/test_skipped.py",
        "import unittest\nraise unittest.SkipTest('not here')\n".into(),
    ));
    files
}

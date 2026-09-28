"""Sphinx configuration for the Ridal documentation."""

project = "Ridal"
author = "Erik Schytt Mannerfelt"
copyright = "Erik Schytt Mannerfelt and contributors"

extensions = [
    "sphinx.ext.autodoc",
    "sphinx.ext.napoleon",
    "myst_parser",
    "sphinx_copybutton",
    "sphinx_design",
]

myst_enable_extensions = [
    "colon_fence",
    "deflist",
]
myst_heading_anchors = 3

# The Python reference is read from an installed `ridal`, whose docstrings
# are NumPy style.
napoleon_google_docstring = False
autodoc_typehints = "none"

exclude_patterns = ["_build", "requirements.txt"]

html_title = "Ridal"
html_static_path = ["_static"]
html_logo = "_static/logo.svg"
html_favicon = "_static/logo.svg"
html_css_files = ["ridal.css"]

html_theme = "furo"
html_theme_options = {
    "source_repository": "https://github.com/erikmannerfelt/ridal",
    "source_branch": "main",
    "source_directory": "docs/",
}

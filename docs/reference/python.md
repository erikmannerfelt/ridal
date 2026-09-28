# Python API

Generated from the docstrings of the installed `ridal` package. The
{doc}`../guide/python` guide shows how the functions fit together.

```{eval-rst}
.. module:: ridal
```

## Reading and inspecting

```{eval-rst}
.. autofunction:: ridal.read

.. autofunction:: ridal.info
```

## Processing

```{eval-rst}
.. autofunction:: ridal.process

.. autofunction:: ridal.batch_process
```

## Rendering

```{eval-rst}
.. autofunction:: ridal.render
```

## Discovery

```{eval-rst}
.. data:: ridal.all_steps
   :type: list[str]

   The names of every processing step. :doc:`steps` describes each one.

.. data:: ridal.all_step_descriptions
   :type: dict[str, str]

   Each step's name mapped to its description, the same text as
   ``ridal steps --describe-all``.

.. data:: ridal.all_formats
   :type: list[str]

   The names of the supported radar formats.

.. data:: ridal.all_format_descriptions
   :type: dict[str, dict]

   Each format's name mapped to its description, its capabilities (whether
   Ridal can ``read`` and ``write`` it) and the file extensions it uses for
   the header, the data and the coordinates.

.. data:: ridal.version
   :type: str

   The installed version of Ridal. ``ridal.__version__`` is the same value.
```

## Removed

```{eval-rst}
.. autofunction:: ridal.run_cli
```

"""The ``step`` object type, for the processing steps reference.

``{step} zero_corr(method=coppens, scope=global)`` renders like a function in
the Python reference: the name, then each argument with its default. It also
gives every step an index entry and a link target, so that other pages can
refer to one with {step}`zero_corr`.
"""

from __future__ import annotations

from docutils import nodes
from sphinx import addnodes
from sphinx.application import Sphinx
from sphinx.domains.std import GenericObject
from sphinx.environment import BuildEnvironment


def parse_step(
    env: BuildEnvironment, signature: str, signode: addnodes.desc_signature
) -> str:
    """Render ``name(arg, [optional], arg=default)`` and return the name.

    The signature is what ``src/steps/mod.rs`` writes, so the arguments are
    separated by ``", "`` and no default contains a comma.
    """
    name, _, rest = signature.partition("(")
    name = name.strip()
    signode += addnodes.desc_name(name, name)
    parameters = addnodes.desc_parameterlist()
    arguments = rest.rstrip(")").strip()
    for argument in arguments.split(", ") if arguments else []:
        optional = argument.startswith("[") and argument.endswith("]")
        argument = argument.strip("[]")
        parameter = addnodes.desc_parameter()
        argument_name, has_default, default = argument.partition("=")
        parameter += addnodes.desc_sig_name(argument_name, argument_name)
        if has_default:
            parameter += addnodes.desc_sig_operator("=", "=")
            parameter += nodes.inline(default, default, classes=["default_value"])
        if optional:
            wrapper = addnodes.desc_optional()
            wrapper += parameter
            parameters += wrapper
        else:
            parameters += parameter
    signode += parameters
    return name


class StepDirective(GenericObject):
    """A step, listed in the page's contents as the Python functions are."""

    indextemplate = "pair: %s; processing step"
    parse_node = staticmethod(parse_step)

    def _object_hierarchy_parts(
        self, sig_node: addnodes.desc_signature
    ) -> tuple[str, ...]:
        return (sig_node.children[0].astext(),)

    def _toc_entry_name(self, sig_node: addnodes.desc_signature) -> str:
        return sig_node.children[0].astext()


def setup(app: Sphinx) -> dict[str, bool]:
    # Registers the object type and the {step} role; the directive is then
    # replaced by the one above, which also adds each step to the contents.
    app.add_object_type("step", "step", objname="processing step")
    app.add_directive_to_domain("std", "step", StepDirective, override=True)
    return {"parallel_read_safe": True}
